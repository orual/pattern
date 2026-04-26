# CLAUDE.md - pattern_server

Daemon server for Pattern, exposing agent runtime over IRPC (QUIC transport).
The binary is `pattern-server`. The CLI manages it via `pattern daemon {start,stop,status}`.

Last verified: 2026-04-26

## Current status

All 6 phases of the v3-TUI plan are complete. v3-sandbox-io wiring is
integrated: `ProjectMount.file_policy` carries per-mount file-access rules
derived from `.pattern.kdl`; the port registry is built via
`PortRegistryImpl::with_runtime_ports` so `HttpPort` is always available.
The daemon provides:

- IRPC-based message routing over QUIC (localhost)
- Actor model: `DaemonServer` owns the event bus and dispatches protocol messages
- Echo mode for CI (no LLM, reflects messages back)
- Real session mode: `TidepoolSession` via `SessionConfig`
- Subscriber fan-out to TUI clients via `TaggedTurnEvent`
- State persistence: `~/.pattern/daemon/state.json` (PID + listen address)
- Shutdown RPC: responds before `std::process::exit(0)`

## Architecture

### Actor model

`DaemonServer` is a tokio actor that runs the server event loop. It owns:

- `recv`: incoming `PatternMessage`s from irpc clients
- `event_rx`: tagged events from `TurnSinkBridge`s (unbounded mpsc)
- `subscribers`: `HashMap<AgentId, Vec<irpc::channel::mpsc::Sender<TaggedTurnEvent>>>`
- `project_mounts`: `Arc<DashMap<PathBuf, Arc<ProjectMount>>>` — cached project mounts
  keyed by canonical path; populated by `InitSession`, used by `SendMessage`.
  Each `ProjectMount` carries `file_policy: Option<FilePolicy>` (always `Some`
  from `get_or_mount_project` per the safe-default contract) derived from the
  mount's `.pattern.kdl` `file_policy {}` block. When the block is absent,
  an empty-rules policy (default-deny) is used.
- `current_mount`: `Option<Arc<ProjectMount>>` — the active project (last `InitSession`
  wins; one project at a time for now)
- `sessions`: `Arc<DashMap<AgentId, AgentSession>>` — shared with spawned tasks so
  session open doesn't block the actor loop
- `session_locks`: `Arc<DashMap<AgentId, Arc<tokio::sync::Mutex<()>>>>` — per-agent
  mutex serializing session open + `set_inner` + step to prevent race conditions on
  concurrent messages
- `batch_to_agent`: `Arc<DashMap<BatchId, AgentId>>` — maps in-flight batch IDs to
  their agent; used by `CancelBatch` to locate the right session. Entries are guarded
  by `BatchGuard` (see below) so they are always removed on task exit.
- `partner_id`: stable `SmolStr` minted once at spawn; all messages from this session
  share one partner identity
- `available_agents`: count of available personas from the last `InitSession`, reported
  by `GetStatus`

Session lifecycle (including tidepool Haskell compilation) runs entirely in spawned
tasks. The actor loop only handles echo mode inline; real-mode `SendMessage`
immediately acknowledges and spawns a task. The free functions `get_or_open_session`
and `resolve_persona` encapsulate session cache logic with double-checked locking.

### `BatchGuard`

A RAII guard held by every spawned session task. When the task exits — normally,
via early return, or panic — the guard's `Drop` removes the `batch_id → agent_id`
entry from `batch_to_agent`. This means the map never leaks even if the task exits
without emitting a `Stop` event. The `fan_out` cleanup on `Stop` is left as a
defensive double-remove; `DashMap::remove` is a no-op when the key is absent.

### IRPC protocol (`protocol.rs`)

Defines `PatternProtocol` (the irpc service) and the message types:

- `InitSession` — TUI handshake: sends project path + preferred agent_id, daemon
  mounts the project on demand and returns `SessionInfo` (resolved agent, persona
  name, available agents)
- `SendMessage` — client sends `AgentMessage`, server acknowledges immediately then
  drives the step
- `SubscribeOutput` — client opens a streaming channel to receive `TaggedTurnEvent`s
- `ListAgents` — returns `Vec<AgentInfo>` (agent_id + persona_name per agent)
- `GetStatus` — returns `RuntimeStatus` (uptime_secs, agent_count, active_batch_count)
- `GetHistory` — returns `HistoryResponse` with historical batches reconstructed from
  stored messages; DB read runs in `spawn_blocking` so the actor loop stays responsive
- `GetClientCount` — returns number of live subscribers; used by `--stop-daemon-on-exit`
- `CancelBatch` — cancels a running step via `CancelState` on the session
- `Shutdown` — responds with `ShutdownResponse` then `std::process::exit(0)` after
  a 50ms delay to let the response flush
- `RunCommand` — transport for plugin-namespaced slash commands (e.g.
  `/plugin-name:do-thing`). Built-in commands route through dedicated RPCs, not here.
  The plugin system is future work; every command currently returns "not implemented".
- `ListCommands` — returns daemon-registered slash commands (empty until plugin system
  lands)

### Event routing (`bridge.rs`)

- `TurnSinkBridge`: per-batch `TurnSink` that tags events with `batch_id` + `agent_id`
  and forwards them on an unbounded mpsc to the daemon actor
- `MultiplexSink`: atomically-swappable `TurnSink` held by each `TidepoolSession`.
  Before each step, the daemon swaps the inner to a fresh `TurnSinkBridge` for that batch.
- Fan-out uses `try_send` — slow subscribers (buffer full) are disconnected rather
  than blocking the actor loop.

### Client (`client.rs`)

`DaemonClient` wraps `irpc::Client<PatternProtocol>` with typed helper methods:
`init_session`, `send_message`, `subscribe_output`, `list_agents`, `get_status`,
`get_history`, `client_count`, `cancel_batch`, `run_command`, `list_commands`,
`shutdown`.

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

Flags passed from `pattern daemon start`:
- `--port N` — QUIC listen port (0 = OS-assigned)
- `--echo` — run in echo mode

Note: `--path` and `--persona` were removed from the CLI's `daemon start` subcommand.
Projects are mounted on demand via `InitSession`; personas are discovered lazily from
`~/.pattern/personas/` and the project mount's `personas/` directory.

## Planned: idle-timeout auto-unmount

The daemon accumulates `project_mounts`, `sessions`, and `session_locks` over time
with no eviction. This is intentional for now — the daemon is meant to be long-lived
and continue operating even when the TUI disconnects.

Planned approach: when automatic agent activation lands, evict mount/session/lock
entries whose agent has been idle beyond a threshold and has no active connection.
Until then, operators should restart the daemon periodically to reclaim resources.
The `--stop-daemon-on-exit` flag on the CLI provides a development-time escape hatch
for flushing all state between sessions.

## v3-sandbox-io wiring

### `ProjectMount.file_policy`

`get_or_mount_project` reads the mount's `.pattern.kdl` `file_policy {}`
section (via `pattern_memory::config::PatternConfig`) and converts it to
a `FilePolicy` via `FilePolicy::from_section()`. If the section is absent,
`FilePolicy::from_rules(Vec::new())` produces a default-deny policy.
The field is always `Some` — the absence sentinel is reserved for callers
that build a `ProjectMount` outside the normal mount path (e.g. test
fixtures). `open_with_agent_loop` receives the policy as a parameter and
constructs the `FileManager` before the eval worker spawns.

### Port registry

`main.rs` builds the port registry via
`PortRegistryImpl::with_runtime_ports(&tokio_handle)` (NOT `::new`) so
`HttpPort` and any future runtime-provided ports are always registered.
The registry is passed to `open_with_agent_loop` for each new session.

## Development guidelines

- Do not run `pattern` or `pattern-server` during development. Production agents
  may be running.
- Use `DaemonServer::spawn()` (echo mode) for in-process integration tests.
- The actor loop must remain non-blocking: all heavy work goes in `tokio::spawn`
  or `tokio::task::spawn_blocking`.
- `fan_out` uses `try_send` to avoid blocking on slow TUI clients.
- Per-agent mutex (`session_locks`) serializes `set_inner` + step to prevent
  batch_id misrouting.
