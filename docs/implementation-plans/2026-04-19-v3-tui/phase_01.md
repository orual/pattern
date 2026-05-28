# v3 TUI — Phase 1: Daemon and IRPC service

**Goal:** A background daemon that owns the agent runtime, exposes it over IRPC (QUIC on localhost), and streams TurnEvents to connected TUI clients.

**Architecture:** The daemon is an actor server built on irpc. It owns a `TidepoolRuntime`, manages open sessions, and broadcasts `TurnEvent`s to subscribers via `tokio::sync::broadcast`. Clients connect over QUIC using a self-signed certificate written to a known path at startup. A state file (`~/.pattern/daemon/state.json`) stores PID and endpoint address for client discovery.

**Tech Stack:** irpc, noq (QUIC), tokio, pattern-runtime, pattern-core

**Scope:** Phase 1 of 6 from the v3-tui design plan.

**Codebase verified:** 2026-04-20

---

## Acceptance criteria coverage

This phase implements and tests:

### v3-tui.AC1: Daemon and IRPC service
- **v3-tui.AC1.1 Success:** `pattern daemon start` starts the daemon; PID file written to `~/.pattern/daemon/`; unix socket created
- **v3-tui.AC1.2 Success:** IRPC test client connects, calls `send_message`, receives `TurnEvent` stream via `subscribe_output`
- **v3-tui.AC1.3 Success:** `pattern daemon status` reports running state, active agent count, socket path
- **v3-tui.AC1.4 Success:** `pattern daemon stop` stops daemon, cleans up PID file and socket
- **v3-tui.AC1.5 Success:** Multiple IRPC clients subscribe to the same agent's output simultaneously; all receive events
- **v3-tui.AC1.6 Failure:** Connecting to a non-existent daemon returns a clear error with instructions to run `pattern daemon start`
- **v3-tui.AC1.7 Edge:** `pattern chat` with no running daemon auto-starts daemon, then connects

**Note:** AC text references "unix socket" from the original design; implementation uses QUIC on localhost with endpoint address discovery via state file. Semantically equivalent.

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->
<!-- START_TASK_1 -->
### Task 1: Workspace and dependency setup

**Files:**
- Modify: `Cargo.toml` (workspace root, line ~8 members array, line ~40+ dependencies)
- Rewrite: `crates/pattern_server/Cargo.toml`
- Delete: all files in `crates/pattern_server/src/` (gut the existing stub)

**Step 1: Update workspace members**

Add `"crates/pattern_server"` to the workspace members array in the root `Cargo.toml` (it's currently missing).

**Step 2: Add workspace dependencies**

Add to `[workspace.dependencies]`:
```toml
irpc = "0.14"
noq = "0.18"
n0-future = "0.3"
```

**Step 3: Rewrite pattern_server Cargo.toml**

```toml
[package]
name = "pattern-server"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true

[[bin]]
name = "pattern-server"
path = "src/main.rs"

[dependencies]
pattern-core = { path = "../pattern_core" }
pattern-runtime = { path = "../pattern_runtime" }
pattern-db = { path = "../pattern_db" }
pattern-memory = { path = "../pattern_memory" }
pattern-provider = { path = "../pattern_provider" }

tokio = { workspace = true, features = ["full"] }
irpc = { workspace = true }
noq = { workspace = true }
n0-future = { workspace = true }

serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
thiserror = { workspace = true }
miette = { workspace = true, features = ["fancy"] }
dirs = { workspace = true }
smol_str = { workspace = true }
clap = { workspace = true }
nix = { version = "0.29", features = ["signal", "process"] }

[dev-dependencies]
tempfile = { workspace = true }
tokio = { workspace = true, features = ["full", "test-util"] }

[lints]
workspace = true
```

**Step 4: Delete existing source files**

Remove everything in `crates/pattern_server/src/`. Create a minimal `src/main.rs`:
```rust
fn main() {
    println!("pattern-server daemon — not yet implemented");
}
```

**Step 5: Create src/lib.rs**

```rust
pub mod bridge;
pub mod client;
pub mod protocol;
pub mod server;
pub mod state;
```

**Step 6: Verify**

Run: `cargo check -p pattern-server`
Expected: compiles (modules are empty stubs, will be populated in subsequent tasks)

**Commit:** `[pattern-server] gut and rebuild as irpc daemon`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Add Serialize/Deserialize to TurnEvent

**Files:**
- Modify: `crates/pattern_core/src/traits/turn_sink.rs` (line 142, TurnEvent derive)

**Step 1: Re-export ContentPart from provider.rs**

In `crates/pattern_core/src/types/provider.rs`, add `ContentPart` to the re-export list at line 45-49:
```rust
pub use genai::chat::{
    CacheControl, ChatMessage, ChatOptions, ChatRequest, ChatResponse, ChatRole, ChatStream,
    ChatStreamEvent, ChatStreamResponse, ContentPart, ReasoningEffort, StreamChunk, StreamEnd,
    SystemBlock, Tool, ToolCall, ToolChunk, ToolResponse, Usage,
};
```

**Step 2: Add serde derives to TurnEvent**

The `TurnEvent` enum at line 142 currently has `#[derive(Debug, Clone)]`. Add `Serialize, Deserialize`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TurnEvent {
    // ... variants unchanged
}
```

All contained types (`ToolCall`, `ToolResult`, `CompletionRequest`, `StopReason`, `DisplayKind`) and their nested genai types all have `Serialize + Deserialize` — verified through the genai fork. This should compile immediately. Verify with `cargo check -p pattern-core`.

**Step 2: Verify**

Run: `cargo check -p pattern-core`
Expected: compiles without errors

Run: `cargo nextest run -p pattern-core turn_sink`
Expected: existing tests still pass

**Commit:** `[pattern-core] add Serialize/Deserialize to TurnEvent for IRPC transport`
<!-- END_TASK_2 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->
<!-- START_TASK_3 -->
### Task 3: IRPC protocol definition

**Verifies:** v3-tui.AC1.2

**Files:**
- Create: `crates/pattern_server/src/protocol.rs`

**Implementation:**

Define the `PatternProtocol` enum using irpc's `#[rpc_requests]` macro. Each variant specifies its response channel type.

```rust
use irpc::{
    channel::{mpsc, oneshot},
    rpc_requests,
};
use pattern_core::traits::turn_sink::TurnEvent;
use pattern_core::types::provider::ContentPart;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Unique identifier for a batch of turn events.
pub type BatchId = SmolStr;
pub type AgentId = SmolStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Client-minted batch ID (snowflake). The daemon uses this to tag all
    /// TurnEvents for this exchange, enabling concurrent batch rendering.
    pub batch_id: BatchId,
    pub agent_id: AgentId,
    /// Message content parts — text, images, binary attachments.
    /// The daemon wraps these into a ChatMessage::user() when constructing TurnInput.
    pub parts: Vec<ContentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSubscription {
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggedTurnEvent {
    pub batch_id: BatchId,
    pub agent_id: AgentId,
    pub event: TurnEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_id: AgentId,
    pub persona_name: String,
    pub active_batches: Vec<BatchId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub agent_count: usize,
    pub active_batch_count: usize,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAgentsRequest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetStatusRequest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashCommand {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub output: String,
}

#[rpc_requests(message = PatternMessage)]
#[derive(Serialize, Deserialize, Debug)]
pub enum PatternProtocol {
    #[rpc(tx = oneshot::Sender<()>)]
    SendMessage(AgentMessage),

    #[rpc(tx = oneshot::Sender<()>)]
    CancelBatch(BatchId),

    #[rpc(tx = mpsc::Sender<TaggedTurnEvent>)]
    SubscribeOutput(AgentSubscription),

    #[rpc(tx = oneshot::Sender<Vec<AgentInfo>>)]
    ListAgents(ListAgentsRequest),

    #[rpc(tx = oneshot::Sender<RuntimeStatus>)]
    GetStatus(GetStatusRequest),

    #[rpc(tx = oneshot::Sender<CommandResult>)]
    RunCommand(SlashCommand),
}

// Note: the design contract includes `set_fronting(Vec<PersonaId>)` as a dedicated RPC.
// This is intentionally simplified to route through `RunCommand` for now. A dedicated
// typed SetFronting variant can be added when multi-agent fronting is implemented.

```

**Testing:**

Test that protocol types round-trip through postcard serialization:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_message_roundtrip() {
        let msg = AgentMessage {
            batch_id: "batch-001".into(),
            agent_id: "agent-1".into(),
            parts: vec![ContentPart::Text("hello".into())],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent_id, "agent-1");
        assert_eq!(decoded.batch_id, "batch-001");
    }

    #[test]
    fn tagged_turn_event_roundtrip() {
        let event = TaggedTurnEvent {
            batch_id: "batch-001".into(),
            agent_id: "agent-1".into(),
            event: TurnEvent::Text("hello world".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: TaggedTurnEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.batch_id, "batch-001");
        assert!(matches!(decoded.event, TurnEvent::Text(ref s) if s == "hello world"));
    }
}
```

**Verification:**

Run: `cargo nextest run -p pattern-server protocol`
Expected: serialization round-trip tests pass

**Commit:** `[pattern-server] define PatternProtocol IRPC service contract`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: TurnSinkBridge

**Verifies:** v3-tui.AC1.2, v3-tui.AC1.5

**Files:**
- Create: `crates/pattern_server/src/bridge.rs`

**Implementation:**

The bridge implements `TurnSink` (synchronous `emit`) and forwards events into a `tokio::sync::broadcast` channel. `broadcast::Sender::send()` is synchronous, satisfying the trait contract. Each subscriber gets a `broadcast::Receiver` from which a forwarding task sends to their irpc mpsc channel.

```rust
use std::sync::Arc;
use std::sync::Mutex;
use pattern_core::traits::turn_sink::{TurnEvent, TurnSink};
use smol_str::SmolStr;

use crate::protocol::TaggedTurnEvent;

/// Single unbounded channel from TurnSink → daemon actor.
/// TurnSinkBridge sends here (sync, lock-free), daemon actor receives
/// and fans out to per-subscriber irpc channels.
pub type EventTx = tokio::sync::mpsc::UnboundedSender<TaggedTurnEvent>;
pub type EventRx = tokio::sync::mpsc::UnboundedReceiver<TaggedTurnEvent>;

pub fn new_event_channel() -> (EventTx, EventRx) {
    tokio::sync::mpsc::unbounded_channel()
}

/// TurnSink implementation that sends tagged events to the daemon actor
/// via an unbounded channel. The daemon actor handles fan-out to subscribers.
///
/// Constructed per-batch: captures the batch_id and agent_id at creation so
/// every emitted event is tagged for routing to the correct subscriber.
///
/// `emit()` is lock-free — `UnboundedSender::send()` uses atomic ops internally.
#[derive(Debug, Clone)]
pub struct TurnSinkBridge {
    batch_id: SmolStr,
    agent_id: SmolStr,
    tx: EventTx,
}

impl TurnSinkBridge {
    pub fn new(batch_id: SmolStr, agent_id: SmolStr, tx: EventTx) -> Self {
        Self { batch_id, agent_id, tx }
    }
}

impl TurnSink for TurnSinkBridge {
    fn emit(&self, event: TurnEvent) {
        let tagged = TaggedTurnEvent {
            batch_id: self.batch_id.clone(),
            agent_id: self.agent_id.clone(),
            event,
        };
        // Lock-free, unbounded, never blocks. Fails only if receiver dropped.
        let _ = self.tx.send(tagged);
    }
}
```

**Testing:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::traits::turn_sink::TurnEvent;
    use pattern_core::types::turn::StopReason;

    #[test]
    fn bridge_emits_tagged_events() {
        let (tx, mut rx) = new_event_channel();
        let bridge = TurnSinkBridge::new("batch-1".into(), "agent-1".into(), tx);

        bridge.emit(TurnEvent::Text("hello".into()));
        bridge.emit(TurnEvent::Stop(StopReason::EndTurn));

        let ev1 = rx.try_recv().unwrap();
        assert_eq!(ev1.batch_id, "batch-1");
        assert_eq!(ev1.agent_id, "agent-1");
        assert!(matches!(ev1.event, TurnEvent::Text(ref s) if s == "hello"));

        let ev2 = rx.try_recv().unwrap();
        assert!(matches!(ev2.event, TurnEvent::Stop(StopReason::EndTurn)));
    }

    #[test]
    fn emit_with_dropped_receiver_does_not_panic() {
        let (tx, _rx) = new_event_channel();
        let bridge = TurnSinkBridge::new("batch-1".into(), "agent-1".into(), tx);
        drop(_rx);
        // Receiver dropped — send fails silently, no panic.
        bridge.emit(TurnEvent::Text("orphaned".into()));
    }
}
```

**Verification:**

Run: `cargo nextest run -p pattern-server bridge`
Expected: all three tests pass

**Commit:** `[pattern-server] TurnSinkBridge: sync TurnSink → broadcast bus`
<!-- END_TASK_4 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 5-6) -->
<!-- START_TASK_5 -->
### Task 5: Daemon server actor

**Verifies:** v3-tui.AC1.1, v3-tui.AC1.2, v3-tui.AC1.5

**Files:**
- Create: `crates/pattern_server/src/server.rs`

**Implementation:**

The daemon server is an actor that receives `PatternMessage`s (generated by the irpc macro) and dispatches them. It owns the event bus and manages session lifecycle.

Key responsibilities:
- Handle `SendMessage`: use the client-minted BatchId from the message, create a `TurnSinkBridge` for that batch, open or reuse a session, spawn a task that drives `step_with_agent_loop`, acknowledge receipt via oneshot
- Handle `SubscribeOutput`: spawn a forwarding task that reads from a `broadcast::Receiver` and sends to the client's irpc mpsc sender, filtering by agent_id
- Handle `ListAgents`, `GetStatus`, `CancelBatch`, `RunCommand` as straightforward lookups/dispatches

For this phase, session management is simplified: the server holds one session per agent_id. Full multi-session lifecycle is deferred to the multi-agent plan.

The server needs a `TidepoolRuntime` and supporting infrastructure (persona loader, provider, db). For now, construction takes these as parameters — the daemon binary (Task 7) handles bootstrapping them.

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use irpc::{Client, WithChannels};
use pattern_core::types::ids::new_snowflake_id;
use smol_str::SmolStr;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::bridge::{EventTx, TurnSinkBridge, new_event_channel};
use crate::protocol::*;

pub struct DaemonServer {
    recv: tokio::sync::mpsc::Receiver<PatternMessage>,
    event_rx: EventRx,
    event_tx: EventTx,
    /// Active subscribers — irpc mpsc senders, one per subscribed TUI client.
    /// Keyed by agent_id for filtering.
    subscribers: Vec<(AgentId, irpc::channel::mpsc::Sender<TaggedTurnEvent>)>,
    started_at: Instant,
}

pub struct DaemonHandle {
    pub client: Client<PatternProtocol>,
}

impl DaemonServer {
    /// Spawn the daemon server actor. Returns a handle for making requests
    /// and the event bus for QUIC listener setup.
    pub fn spawn() -> DaemonHandle {
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel(64);
        let (event_tx, event_rx) = new_event_channel();
        let server = Self {
            recv: msg_rx,
            event_rx,
            event_tx,
            subscribers: Vec::new(),
            started_at: Instant::now(),
        };
        tokio::spawn(server.run());
        DaemonHandle {
            client: Client::local(msg_tx),
        }
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                msg = self.recv.recv() => {
                    match msg {
                        Some(msg) => self.handle(msg).await,
                        None => break, // All senders dropped.
                    }
                }
                event = self.event_rx.recv() => {
                    if let Some(event) = event {
                        self.fan_out(event).await;
                    }
                }
            }
        }
    }

    /// Fan out a tagged event to all matching subscribers.
    /// Removes subscribers whose channels are closed.
    async fn fan_out(&mut self, event: TaggedTurnEvent) {
        let mut i = 0;
        while i < self.subscribers.len() {
            let (ref agent_filter, ref tx) = self.subscribers[i];
            if *agent_filter == event.agent_id {
                if tx.send(event.clone()).await.is_err() {
                    // Subscriber disconnected — remove.
                    self.subscribers.swap_remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }

    async fn handle(&mut self, msg: PatternMessage) {
        match msg {
            PatternMessage::SendMessage(req) => {
                let WithChannels { tx, inner, .. } = req;
                let batch_id = inner.batch_id.clone();
                // Acknowledge receipt.
                let _ = tx.send(()).await;
                // TODO: drive session step with TurnSinkBridge.
                // For now, emit a synthetic Text + Stop to prove the bus works.
                let bridge = TurnSinkBridge::new(
                    batch_id,
                    inner.agent_id,
                    self.event_tx.clone(),
                );
                use pattern_core::traits::turn_sink::TurnSink;
                let text = inner.parts.iter()
                    .filter_map(|p| match p {
                        ContentPart::Text(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                bridge.emit(pattern_core::traits::turn_sink::TurnEvent::Text(
                    format!("echo: {text}"),
                ));
                bridge.emit(pattern_core::traits::turn_sink::TurnEvent::Stop(
                    pattern_core::types::turn::StopReason::EndTurn,
                ));
            }
            PatternMessage::SubscribeOutput(req) => {
                let WithChannels { tx, inner, .. } = req;
                // Register this subscriber. The actor's fan_out() method
                // will forward matching events to this irpc mpsc sender.
                self.subscribers.push((inner.agent_id, tx));
            }
            PatternMessage::ListAgents(req) => {
                let WithChannels { tx, .. } = req;
                // TODO: return actual agent list from runtime.
                let _ = tx.send(vec![]).await;
            }
            PatternMessage::GetStatus(req) => {
                let WithChannels { tx, .. } = req;
                let status = RuntimeStatus {
                    agent_count: 0,
                    active_batch_count: 0,
                    uptime_secs: self.started_at.elapsed().as_secs(),
                };
                let _ = tx.send(status).await;
            }
            PatternMessage::CancelBatch(req) => {
                let WithChannels { tx, .. } = req;
                // TODO: cancel via session CancelState.
                let _ = tx.send(()).await;
            }
            PatternMessage::RunCommand(req) => {
                let WithChannels { tx, inner, .. } = req;
                let result = CommandResult {
                    success: false,
                    output: format!("command not yet implemented: {}", inner.command),
                };
                let _ = tx.send(result).await;
            }
        }
    }
}
```

**Testing:**

Integration test verifying send_message + subscribe_output flow:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Note: these tests use DaemonClient from Task 6. Implement Tasks 5 and 6
    // together before running tests, as they form subcomponent C.

    #[tokio::test]
    async fn send_message_returns_batch_id_and_emits_events() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        // Subscribe before sending.
        let mut events = client.subscribe_output("test-agent".into()).await.unwrap();

        // Send a message (client mints the batch_id).
        let batch_id: SmolStr = new_snowflake_id();
        client.send_message(batch_id.clone(), "test-agent".into(), vec![ContentPart::Text("hello".into())]).await.unwrap();

        // Receive events — tagged with our batch_id.
        let ev = events.recv().await.unwrap().unwrap();
        assert_eq!(ev.batch_id, batch_id);
        assert!(matches!(ev.event, TurnEvent::Text(ref s) if s.contains("hello")));
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_same_events() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        let mut rx1 = client.subscribe_output("test-agent".into()).await.unwrap();
        let mut rx2 = client.subscribe_output("test-agent".into()).await.unwrap();

        let batch_id: SmolStr = new_snowflake_id();
        client.send_message(batch_id.clone(), "test-agent".into(), vec![ContentPart::Text("shared".into())]).await.unwrap();

        let ev1 = rx1.recv().await.unwrap().unwrap();
        let ev2 = rx2.recv().await.unwrap().unwrap();
        assert_eq!(ev1.batch_id, batch_id);
        assert_eq!(ev2.batch_id, batch_id);
    }

    #[tokio::test]
    async fn get_status_returns_uptime() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        let status = client.get_status().await.unwrap();
        assert_eq!(status.agent_count, 0);
    }
}
```

**Verification:**

Run: `cargo nextest run -p pattern-server server`
Expected: all tests pass

**Commit:** `[pattern-server] daemon server actor with IRPC dispatch`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: DaemonClient wrapper

**Verifies:** v3-tui.AC1.2, v3-tui.AC1.6

**Files:**
- Create: `crates/pattern_server/src/client.rs`

**Implementation:**

Wraps `Client<PatternProtocol>` with typed methods. Handles both local (in-process) and remote (QUIC) construction.

```rust
use irpc::channel::mpsc;
use irpc::Client;
use smol_str::SmolStr;
use thiserror::Error;

use crate::protocol::*;
use crate::state::DaemonState;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DaemonClientError {
    #[error("daemon not running — start it with `pattern daemon start`")]
    DaemonNotRunning,

    #[error("failed to connect to daemon at {addr}: {source}")]
    ConnectionFailed {
        addr: String,
        source: std::io::Error,
    },

    #[error("rpc request failed: {0}")]
    Rpc(#[from] irpc::RequestError),

    #[error("failed to read daemon state: {0}")]
    StateRead(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DaemonClientError>;

pub struct DaemonClient {
    inner: Client<PatternProtocol>,
}

impl DaemonClient {
    /// Create a client from a local channel (in-process, for testing).
    pub fn from_local(client: Client<PatternProtocol>) -> Self {
        Self { inner: client }
    }

    /// Connect to a running daemon by reading its state file.
    pub async fn connect() -> Result<Self> {
        let state = DaemonState::load()
            .map_err(|_| DaemonClientError::DaemonNotRunning)?;

        if !state.is_process_alive() {
            return Err(DaemonClientError::DaemonNotRunning);
        }

        let cert = state.load_cert()
            .map_err(|e| DaemonClientError::ConnectionFailed {
                addr: state.addr.to_string(),
                source: e,
            })?;

        let endpoint = irpc::util::make_client_endpoint(
            std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
            &[&cert],
        ).map_err(|e| DaemonClientError::ConnectionFailed {
            addr: state.addr.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
        })?;

        Ok(Self {
            inner: Client::noq(endpoint, state.addr),
        })
    }

    pub async fn send_message(&self, batch_id: SmolStr, agent_id: SmolStr, parts: Vec<ContentPart>) -> Result<()> {
        self.inner.rpc(AgentMessage { batch_id, agent_id, parts }).await?;
        Ok(())
    }

    pub async fn subscribe_output(&self, agent_id: SmolStr) -> Result<mpsc::Receiver<TaggedTurnEvent>> {
        let rx = self.inner
            .server_streaming(AgentSubscription { agent_id }, 64)
            .await?;
        Ok(rx)
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentInfo>> {
        let agents = self.inner.rpc(ListAgentsRequest).await?;
        Ok(agents)
    }

    pub async fn get_status(&self) -> Result<RuntimeStatus> {
        let status = self.inner.rpc(GetStatusRequest).await?;
        Ok(status)
    }

    pub async fn cancel_batch(&self, batch_id: SmolStr) -> Result<()> {
        self.inner.rpc(batch_id).await?;
        Ok(())
    }

    pub async fn run_command(&self, command: String, args: Vec<String>) -> Result<CommandResult> {
        let result = self.inner.rpc(SlashCommand { command, args }).await?;
        Ok(result)
    }
}
```

**Testing:**

Error path test:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_without_daemon_returns_clear_error() {
        // Ensure no state file exists (use temp dir).
        let result = DaemonClient::connect().await;
        assert!(matches!(result, Err(DaemonClientError::DaemonNotRunning)));
    }
}
```

**Verification:**

Run: `cargo nextest run -p pattern-server client`
Expected: error path test passes

**Commit:** `[pattern-server] DaemonClient with local and QUIC connection modes`
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 7-8) -->
<!-- START_TASK_7 -->
### Task 7: Daemon state management

**Verifies:** v3-tui.AC1.1, v3-tui.AC1.3, v3-tui.AC1.4

**Files:**
- Create: `crates/pattern_server/src/state.rs`

**Implementation:**

Manages the daemon's state file and certificate at `~/.pattern/daemon/`.

```rust
use std::net::SocketAddr;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    pub pid: u32,
    pub addr: SocketAddr,
}

impl DaemonState {
    /// Directory where daemon state is stored.
    /// Overridable via `PATTERN_STATE_DIR` env var for testing.
    pub fn state_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("PATTERN_STATE_DIR") {
            return PathBuf::from(dir);
        }
        dirs::home_dir()
            .expect("home directory must exist")
            .join(".pattern")
            .join("daemon")
    }

    /// Path to the state JSON file.
    pub fn state_path() -> PathBuf {
        Self::state_dir().join("state.json")
    }

    /// Path to the self-signed certificate (DER format).
    pub fn cert_path() -> PathBuf {
        Self::state_dir().join("cert.der")
    }

    /// Write state and certificate to disk.
    pub fn save(&self, cert_der: &[u8]) -> std::io::Result<()> {
        let dir = Self::state_dir();
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(Self::state_path(), json)?;
        std::fs::write(Self::cert_path(), cert_der)?;
        Ok(())
    }

    /// Load state from disk. Returns error if file doesn't exist.
    pub fn load() -> std::io::Result<Self> {
        let json = std::fs::read_to_string(Self::state_path())?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Load the certificate DER from disk.
    pub fn load_cert(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(Self::cert_path())
    }

    /// Remove state and cert files.
    pub fn clear() -> std::io::Result<()> {
        let _ = std::fs::remove_file(Self::state_path());
        let _ = std::fs::remove_file(Self::cert_path());
        Ok(())
    }

    /// Check if the process at self.pid is still alive.
    pub fn is_process_alive(&self) -> bool {
        use nix::sys::signal;
        use nix::unistd::Pid;
        // kill(pid, None) checks existence without signalling.
        signal::kill(Pid::from_raw(self.pid as i32), None).is_ok()
    }
}
```

Uses `nix` crate for safe process signalling (added to Cargo.toml in Task 1).

**Testing:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[test]
    fn state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        // Override state_dir for test — use env var or pass dir.
        // For unit test, test serialization directly:
        let state = DaemonState {
            pid: 12345,
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9847).into(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let decoded: DaemonState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.pid, 12345);
    }

    #[test]
    fn is_process_alive_returns_false_for_nonexistent() {
        let state = DaemonState {
            pid: 99999999, // Almost certainly not running.
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1).into(),
        };
        assert!(!state.is_process_alive());
    }
}
```

**Verification:**

Run: `cargo nextest run -p pattern-server state`
Expected: tests pass

**Commit:** `[pattern-server] daemon state file management`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Daemon binary entry point

**Verifies:** v3-tui.AC1.1, v3-tui.AC1.3, v3-tui.AC1.4

**Files:**
- Rewrite: `crates/pattern_server/src/main.rs`

**Implementation:**

The daemon binary handles `start`, `stop`, and `status` subcommands. On `start`, it spawns the server actor, creates a QUIC endpoint, writes state, and blocks until signalled.

```rust
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use clap::{Parser, Subcommand};
use tracing::info;

use pattern_server::server::DaemonServer;
use pattern_server::state::DaemonState;

#[derive(Parser)]
#[command(name = "pattern-server", about = "Pattern daemon process")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon.
    Start {
        /// Port to listen on (0 = OS-assigned).
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    /// Stop a running daemon.
    Stop,
    /// Show daemon status.
    Status,
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("pattern_server=info")
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Start { port } => cmd_start(port).await,
        Command::Stop => cmd_stop(),
        Command::Status => cmd_status(),
    }
}

async fn cmd_start(port: u16) -> miette::Result<()> {
    // Check if already running.
    if let Ok(state) = DaemonState::load() {
        if state.is_process_alive() {
            return Err(miette::miette!(
                "daemon already running (pid {}, addr {})",
                state.pid,
                state.addr
            ));
        }
        // Stale state file — clean it up.
        DaemonState::clear().ok();
    }

    // Spawn the server actor.
    let handle = DaemonServer::spawn();

    // Create QUIC endpoint.
    let bind_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into();
    let (endpoint, cert_der) = irpc::util::make_server_endpoint(bind_addr)
        .map_err(|e| miette::miette!("failed to create QUIC endpoint: {e}"))?;

    let local_addr = endpoint.local_addr()
        .map_err(|e| miette::miette!("failed to get local addr: {e}"))?;

    // Start listening for remote connections.
    let local = handle.client.as_local()
        .expect("server must be local");
    let _listener = tokio::spawn(irpc::rpc::listen(
        endpoint,
        PatternProtocol::remote_handler(local),
    ));

    // Write state.
    let state = DaemonState {
        pid: std::process::id(),
        addr: local_addr,
    };
    state.save(&cert_der)
        .map_err(|e| miette::miette!("failed to write state: {e}"))?;

    info!("daemon listening on {}", local_addr);
    info!("state written to {}", DaemonState::state_path().display());

    // Block until ctrl-c.
    tokio::signal::ctrl_c().await.ok();

    info!("shutting down");
    DaemonState::clear().ok();

    Ok(())
}

fn cmd_stop() -> miette::Result<()> {
    let state = DaemonState::load()
        .map_err(|_| miette::miette!("daemon not running (no state file)"))?;

    if !state.is_process_alive() {
        DaemonState::clear().ok();
        return Err(miette::miette!("daemon not running (stale state file cleaned up)"));
    }

    // Send SIGTERM via nix (safe wrapper).
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;
    let _ = signal::kill(Pid::from_raw(state.pid as i32), Signal::SIGTERM);
    DaemonState::clear().ok();
    println!("daemon stopped (pid {})", state.pid);
    Ok(())
}

fn cmd_status() -> miette::Result<()> {
    let state = match DaemonState::load() {
        Ok(s) => s,
        Err(_) => {
            println!("daemon not running");
            return Ok(());
        }
    };

    if !state.is_process_alive() {
        DaemonState::clear().ok();
        println!("daemon not running (stale state file cleaned up)");
        return Ok(());
    }

    println!("daemon running");
    println!("  pid:  {}", state.pid);
    println!("  addr: {}", state.addr);
    Ok(())
}
```

**Testing:**

The binary is tested via manual smoke test:
1. `cargo build -p pattern-server`
2. `./target/debug/pattern-server start` (runs in foreground)
3. In another terminal: `./target/debug/pattern-server status`
4. Ctrl-C the first terminal
5. `./target/debug/pattern-server status` shows "not running"

**Verification:**

Run: `cargo build -p pattern-server`
Expected: compiles without errors

**Commit:** `[pattern-server] daemon binary with start/stop/status commands`
<!-- END_TASK_8 -->
<!-- END_SUBCOMPONENT_D -->

<!-- START_TASK_9 -->
### Task 9: Wire SendMessage to real TidepoolSession

**Verifies:** v3-tui.AC1.2

**Files:**
- Modify: `crates/pattern_server/src/server.rs`
- Modify: `crates/pattern_server/src/main.rs`

**Implementation:**

Replace the echo-mode stub in the SendMessage handler with actual session integration. The daemon bootstraps a `TidepoolRuntime` at startup and uses it to open sessions and drive steps.

Daemon startup (in `main.rs` `cmd_start`):
1. Determine project path from `--path` flag or current directory
2. Attach to mount: `pattern_memory::mount::attach(project_path)` → `MountedStore`
   - This handles mode resolution (A/B/C), DB path discovery, opens both memory.db + messages.db via `ConstellationDb::open()`, and creates `MemoryCache`
3. Resolve provider credentials via `AnthropicAuthChain` (following `pattern-test-cli` pattern)
4. Create `PatternGatewayClient` as the `ProviderClient`
5. Locate SDK via `SdkLocation::detect()` or default
6. Construct `TidepoolRuntime::new(sdk, mounted.cache.clone(), provider, mounted.db.clone())`
   - `mounted.cache` is the `Arc<MemoryCache>` (implements `MemoryStore`)
   - `mounted.db` is the `Arc<ConstellationDb>` (manages both memory.db + messages.db)
7. Pass runtime to `DaemonServer::spawn(runtime)`
8. Keep `MountedStore` alive for the daemon's lifetime (it owns the watcher and backup scheduler)

`DaemonServer` gains a `runtime: Arc<TidepoolRuntime>` field and a `sessions: HashMap<AgentId, Arc<TidepoolSession>>` for open sessions.

Updated SendMessage handler (replacing echo stub):
```rust
PatternMessage::SendMessage(req) => {
    let WithChannels { tx, inner, .. } = req;
    let batch_id = inner.batch_id.clone();
    let agent_id = inner.agent_id.clone();

    // Acknowledge receipt.
    let _ = tx.send(()).await;

    // Build TurnSinkBridge for this batch.
    let bridge = Arc::new(TurnSinkBridge::new(
        batch_id, agent_id.clone(), self.event_tx.clone(),
    ));

    // Open or reuse session for this agent.
    let session = self.get_or_open_session(&agent_id, bridge.clone()).await;

    // Build TurnInput from client's message parts.
    let turn_input = build_turn_input(&inner);

    // Drive step in background task.
    tokio::spawn(async move {
        match session.step_with_agent_loop(turn_input).await {
            Ok(_reply) => { /* events already emitted via bridge */ }
            Err(e) => {
                bridge.emit(TurnEvent::Display {
                    kind: DisplayKind::Note,
                    text: format!("error: {e}"),
                });
                bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
            }
        }
    });
}
```

The `build_turn_input` helper constructs a `TurnInput` from `AgentMessage`:
- Mint turn_id via `new_snowflake_id()`
- Use the client-provided batch_id
- Build `MessageOrigin` with `Author::Partner`
- Wrap parts into a `ChatMessage::user(MessageContent::from_parts(parts))`

Session management: `get_or_open_session` checks the sessions map, opens a new session via `runtime.open_session()` if not found. The TurnSink on the session is replaced with the per-batch bridge for each new message (via `SessionContext::with_turn_sink`).

**Persona loading:** The daemon accepts a `--persona` flag specifying the default persona KDL file. Load via `pattern_runtime::persona_loader` (which parses KDL persona definitions into `PersonaSnapshot`). The `PersonaSnapshot` is passed to `runtime.open_session(persona, None)` to open a session. Follow `pattern-test-cli`'s spawn flow as the reference implementation — it demonstrates the full persona → session → step lifecycle. Multi-agent persona discovery is deferred to the multi-agent plan.

**Testing:**

This task transforms the integration tests from echo-mode to real-session-mode. In CI without provider credentials, tests should still pass using the echo fallback or a mock provider. Add a `--echo` flag to the daemon for testing that preserves the echo behaviour.

- Existing integration tests (Task 11) continue to work in echo mode
- Manual smoke test: start daemon with `--persona path/to/persona.toml`, send message via test client, receive real LLM response

**Verification:**

Run: `cargo build -p pattern-server`
Expected: compiles

Run: `cargo nextest run -p pattern-server`
Expected: tests pass (echo mode for CI)

**Commit:** `[pattern-server] wire SendMessage to TidepoolSession step`
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: CLI daemon subcommand

**Verifies:** v3-tui.AC1.1, v3-tui.AC1.3, v3-tui.AC1.4, v3-tui.AC1.7

**Files:**
- Modify: `crates/pattern_cli/src/main.rs` (add Daemon variant to Commands enum)
- Create: `crates/pattern_cli/src/commands/daemon.rs`
- Modify: `crates/pattern_cli/src/commands.rs` (add module declaration)
- Modify: `crates/pattern_cli/Cargo.toml` (add pattern-server dependency)

**Implementation:**

Add a `Daemon` subcommand to the CLI that delegates to the `pattern-server` binary (or connects to a running daemon for status).

In `commands/daemon.rs`:
- `Start`: spawn `pattern-server start` as a detached child process
- `Stop`: read state file, send SIGTERM (or shell out to `pattern-server stop`)
- `Status`: read state file, report

In `main.rs`, add:
```rust
/// Manage the Pattern daemon.
Daemon(DaemonCmd),
```

For AC1.7 (`pattern chat` auto-starts daemon): add a helper `ensure_daemon_running()` that checks state file, starts daemon if not running, waits briefly for state file to appear, then connects. Wire this into the TUI startup path (Phase 2 will use it).

**Testing:**

Command parsing is verified by clap derives. Integration tested manually.

**Verification:**

Run: `cargo build -p pattern-cli`
Expected: compiles with the new subcommand

**Commit:** `[pattern-cli] add daemon start/stop/status subcommands`
<!-- END_TASK_10 -->

<!-- START_TASK_11 -->
### Task 11: End-to-end integration test

**Verifies:** v3-tui.AC1.2, v3-tui.AC1.5

**Files:**
- Create: `crates/pattern_server/tests/integration.rs`

**Implementation:**

A full integration test that exercises the IRPC contract in-process (no QUIC, using local channels):

```rust
use pattern_server::server::DaemonServer;
use pattern_server::client::DaemonClient;
use pattern_core::traits::turn_sink::TurnEvent;

#[tokio::test]
async fn full_send_subscribe_flow() {
    let handle = DaemonServer::spawn();
    let client = DaemonClient::from_local(handle.client);

    // Subscribe to an agent's output.
    let mut events = client.subscribe_output("agent-1".into()).await.unwrap();

    // Send a message.
    let batch_id: SmolStr = new_snowflake_id();
    client.send_message(batch_id.clone(), "agent-1".into(), vec![ContentPart::Text("what is 2+2?".into())]).await.unwrap();

    // Collect events until Stop.
    let mut received = vec![];
    loop {
        let ev = events.recv().await.unwrap().unwrap();
        let is_stop = matches!(ev.event, TurnEvent::Stop(_));
        received.push(ev);
        if is_stop { break; }
    }

    // Verify batch tagging.
    assert!(received.iter().all(|e| e.batch_id == batch_id));
    assert!(received.iter().all(|e| e.agent_id == "agent-1"));

    // Verify we got Text + Stop at minimum.
    assert!(received.iter().any(|e| matches!(e.event, TurnEvent::Text(_))));
    assert!(matches!(received.last().unwrap().event, TurnEvent::Stop(_)));
}

#[tokio::test]
async fn subscriber_filtering_by_agent() {
    let handle = DaemonServer::spawn();
    let client = DaemonClient::from_local(handle.client);

    // Subscribe only to agent-1.
    let mut rx = client.subscribe_output("agent-1".into()).await.unwrap();

    // Send to agent-2 — subscriber should not receive it.
    client.send_message(new_snowflake_id(), "agent-2".into(), vec![ContentPart::Text("hello".into())]).await.unwrap();

    // Send to agent-1 — subscriber should receive it.
    client.send_message(new_snowflake_id(), "agent-1".into(), vec![ContentPart::Text("hello".into())]).await.unwrap();

    let ev = rx.recv().await.unwrap().unwrap();
    assert_eq!(ev.agent_id, "agent-1");
}
```

**Verification:**

Run: `cargo nextest run -p pattern-server --test integration`
Expected: both tests pass

**Commit:** `[pattern-server] end-to-end integration tests for IRPC service`
<!-- END_TASK_11 -->
