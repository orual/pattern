# CLAUDE.md - pattern_server

Daemon server for Pattern, exposing agent runtime over IRPC (QUIC transport).
The binary is `pattern-server`. The CLI manages it via `pattern daemon {start,stop,status}`.

Last verified: 2026-04-21

## Current status

Phase 1 of the v3-TUI plan is complete. The daemon provides:

- IRPC-based message routing over QUIC (localhost)
- Actor model: `DaemonServer` owns the event bus and dispatches protocol messages
- Echo mode for CI (no LLM, reflects messages back)
- Real session mode: `TidepoolSession` via `SessionConfig`
- Subscriber fan-out to TUI clients via `TaggedTurnEvent`
- State persistence: `~/.pattern/daemon/state.json` (PID + listen address)

## Architecture

### Actor model

`DaemonServer` is a tokio actor that runs the server event loop. It owns:

- `recv`: incoming `PatternMessage`s from irpc clients
- `event_rx`: tagged events from `TurnSinkBridge`s (unbounded mpsc)
- `subscribers`: `HashMap<AgentId, Vec<irpc::channel::mpsc::Sender<TaggedTurnEvent>>>`
- `project_mounts`: `Arc<DashMap<PathBuf, Arc<ProjectMount>>>` — cached project mounts keyed by canonical path; populated by `InitSession`, used by `SendMessage`
- `current_mount`: `Option<Arc<ProjectMount>>` — the active project (last `InitSession` wins; one project at a time for now)
- `sessions`: `Arc<DashMap<AgentId, AgentSession>>` — shared with spawned tasks so session open doesn't block the actor loop
- `session_locks`: `Arc<DashMap<AgentId, Arc<tokio::sync::Mutex<()>>>>` — per-agent mutex serializing session open + `set_inner` + step to prevent race conditions on concurrent messages
- `partner_id`: stable `SmolStr` minted once at spawn; all messages from this session share one partner identity

Session lifecycle (including tidepool Haskell compilation) runs entirely in
spawned tasks. The actor loop only handles echo mode inline; real-mode
`SendMessage` immediately acknowledges and spawns a task. The free functions
`get_or_open_session` and `resolve_persona` encapsulate session cache logic
with double-checked locking.

### IRPC protocol (`protocol.rs`)

Defines `PatternProtocol` (the irpc service) and the message types:

- `InitSession` — TUI handshake: sends project path + preferred agent_id, daemon mounts the project on demand and returns `SessionInfo` (resolved agent, persona name, available agents)
- `SendMessage` — client sends `AgentMessage`, server acknowledges immediately then drives the step
- `SubscribeOutput` — client opens a streaming channel to receive `TaggedTurnEvent`s
- `ListAgents` — returns `Vec<AgentInfo>`
- `GetStatus` — returns `RuntimeStatus` (uptime, agent count)
- `CancelBatch` — phase 2: will cancel a running step via `CancelState`
- `RunCommand` — reserved for future use

### Event routing (`bridge.rs`)

- `TurnSinkBridge`: per-batch `TurnSink` that tags events with `batch_id` + `agent_id` and forwards them on an unbounded mpsc to the daemon actor
- `MultiplexSink`: atomically-swappable `TurnSink` held by each `TidepoolSession`. Before each step, the daemon swaps the inner to a fresh `TurnSinkBridge` for that batch.
- Fan-out uses `try_send` — slow subscribers (buffer full) are disconnected rather than blocking the actor loop.

### Client (`client.rs`)

`DaemonClient` wraps `irpc::Client<PatternProtocol>` with typed helper methods:
`init_session`, `send_message`, `subscribe_output`, `list_agents`, `get_status`.

### State (`state.rs`)

`DaemonState` persists `{ pid, addr }` to `~/.pattern/daemon/state.json` (or
`$PATTERN_STATE_DIR/state.json` for tests). `is_process_alive()` checks via `kill(pid, 0)`.

## Module overview

```
src/
├── bridge.rs    # TurnSinkBridge, MultiplexSink, event channel types
├── client.rs    # DaemonClient (typed irpc client)
├── main.rs      # pattern-server binary entry point
├── protocol.rs  # PatternProtocol, PatternMessage, TaggedTurnEvent, request/response types
├── server.rs    # DaemonServer actor
└── state.rs     # DaemonState (PID + addr persistence)
```

## Testing

Echo mode is designed for CI. Tests in `server.rs` and `bridge.rs` use it.

```bash
# Run all tests for this crate
cargo nextest run -p pattern-server

# With output
cargo nextest run -p pattern-server --nocapture
```

No external services needed — echo mode runs without LLM credentials.

## CLI integration

The `pattern-cli` crate manages the daemon process via `pattern daemon {start,stop,status}`.
The CLI finds `pattern-server` as a sibling binary (same directory) or via `PATH`.

Forwarded flags from `pattern daemon start`:
- `--port N` — QUIC listen port (0 = OS-assigned)
- `--echo` — run in echo mode
- `--path DIR` — (legacy, ignored) project root; projects are now mounted on demand via `InitSession`
- `--persona PATH` — (legacy, ignored) persona KDL file; personas are discovered lazily

## Development guidelines

- Do not run `pattern` or `pattern-server` during development. Production agents may be running.
- Use `DaemonServer::spawn()` (echo mode) for in-process integration tests.
- The actor loop must remain non-blocking: all heavy work goes in `tokio::spawn`.
- `fan_out` uses `try_send` to avoid blocking on slow TUI clients.
- Per-agent mutex (`session_locks`) serializes `set_inner` + step to prevent batch_id misrouting.
