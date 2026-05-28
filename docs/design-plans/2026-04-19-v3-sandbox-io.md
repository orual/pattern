# Pattern v3 Sandbox I/O Design

## Summary

Pattern's agents interact with the outside world through SDK effect handlers — typed Haskell-side effect constructors that dispatch to Rust runtime coordinators via a request-response model. This plan implements the remaining stub handlers for shell execution, filesystem access, and external service interaction. The shell handler wraps a persistent PTY session (ported from v2 reference code), giving agents the ability to run commands, spawn long-running processes, and receive async output as system reminders. The file handler wraps filesystem access behind CRDT-backed sync primitives (`LoroSyncedFile`), so that an agent editing a file and an external process editing the same file simultaneously can both have their changes preserved; files the agent is not actively editing are read or watched without the overhead of CRDT tracking. The Port handler replaces two stale stubs (Sources and Rpc) with a single trait that gives agents a uniform call/subscribe interface to any external service, with an optional Haskell library capability for ergonomic typed access.

The unifying design principle is the coordinator pattern: handlers are stateless dispatchers; the stateful objects (PTY sessions, open LoroSyncedFile instances, registered port implementations) are owned by long-lived runtime coordinators (ProcessManager, FileManager, PortRegistry) behind Arc. Async notifications from all three subsystems — file change diffs, shell spawn output, and port event streams — flow through the same system reminder mechanism that Plan 1 established for memory block changes, keeping the agent's notification model uniform. Permission gating for all three handlers slots into Plan 3's capability system. The Port trait defined here becomes a prereq for Plan 4 (v3-extensibility), which uses it for plugin-registered external services.

## Definition of Done

Implements the remaining stub SDK effect handlers for agent interaction with the outside world — shell command execution, filesystem access, and data source resolution. The plan is done when:

### Shell handler

- `ctx.shell.execute/spawn/kill/status` fully implemented in `crates/pattern_runtime/src/sdk/handlers/shell.rs` via a PTY backend (ported from v2 `rewrite-staging/runtime_subsystems/data_source/process/` and `tool/builtin/shell.rs` reference code)
- Supports persistent shell sessions (agent can spawn a shell, run commands, inspect output, kill it)
- Subprocess lifecycle management: spawn, signal handling, exit code capture, stdout/stderr streaming
- Permission-gated via Plan 3's capability system (Shell effect category in CapabilitySet, destructive command policy in runtime approval layer)

### File handler

- `ctx.file.read/write/list` fully implemented in `crates/pattern_runtime/src/sdk/handlers/file.rs`
- Selective CRDT wrapping: files opened for editing get a LoroDoc for merge/rewind/conflict resolution (ported from v2 `rewrite-staging/runtime_subsystems/data_source/file_source.rs` reference code). Files only read do not get a LoroDoc.
- `LoroSyncedFile` shared infrastructure extracted from Plan 1's subscriber code — LoroDoc creation, merge logic, self-emit-echo detection reused by both block subscribers and FileManager
- LoroSyncedFile is ephemeral and in-memory only — no jj integration, no sqlite index, no block metadata. dies on file close or session end. the file on disk is the only survivor.
- Notify-watcher for external edits to CRDT-wrapped files, reconciled via loro merge
- File watching delivers diff updates as system reminders between turns (agent notified of what changed, self-echo filtered)
- Permission-gated via Plan 3's capability system (File effect category in CapabilitySet). Config file writes gated by shape-based detection (from Plan 3's design — writes to files that parse as pattern config KDL always require human approval)
- Directory scoping: agent's file access restricted to permitted paths (project root, mount directories, explicitly allowed paths). KDL config supports both allow and deny rules; deny evaluated first.
- Session state serialization: which files were open (paths only) recorded so a resumed session can re-open them

### Port effect (replaces Sources + Rpc)

- Sources and Rpc stubs replaced by a single **Port** effect — the agent's unified interface to external services
- `Port` trait defined in `pattern_core` with: `id()`, `metadata()`, `subscribe(config)`, `call(method, payload)`, `capabilities()`, `library()`
- `library()` returns optional Haskell helper source compiled into the agent's prelude when the port is available — typed wrappers for the port's API so agents get ergonomic access
- SDK surface: `ctx.port.list()`, `ctx.port.call(id, method, payload)`, `ctx.port.subscribe(id, config)`, `ctx.port.unsubscribe(id)`
- Configuration via convention: `call("configure", config)` rather than a separate trait method
- Runtime-provided ports ship with pattern (e.g., `http` for one-shot HTTP requests)
- Plugin-registered ports consumed by Plan 4's plugin system (Plan 4 depends on Port trait from this plan)
- `DataStream` trait and `SourceManager` trait retired in favour of `Port` trait and `PortRegistry`
- MCP stays as a separate SDK effect (Plan 4) — potential unification with Port is a future consideration

### Integration

- All handlers integrate with Plan 3's capability system (prelude filtering for effect visibility, runtime approval for per-invocation policy)
- Plan 3's capability system references to Shell and File effects work end-to-end
- No stub handlers remain in `crates/pattern_runtime/src/sdk/handlers/` except Spawn (Plan 3) and Mcp (Plan 4)
- Sources and Rpc handler stubs removed (replaced by Port)

### Testing

- Shell handler: deterministic tests for spawn/execute/kill/status lifecycle (temp PTY, no external commands in CI beyond basic shell builtins)
- File handler: deterministic tests for read/write/list + CRDT merge scenarios (temp directories, concurrent edit simulation)
- Permission gating tests for both handlers (capability filtering, policy enforcement)
- No live-model dependency in CI

### Explicitly OUT OF SCOPE

- Spawn handler (Plan 3: v3-multi-agent)
- Mcp handler (Plan 4: v3-extensibility)
- Plugin-registered ports (Plan 4 consumes Port trait from this plan)
- Message.Ask implementation (deferred, wire up separately)
- TUI rendering of shell output or file diffs

### Context

This plan fills the remaining SDK handler stubs that are independent of Plans 3 and 4. Depends on:

- `docs/design-plans/2026-04-19-v3-memory-rework.md` (Plan 1 — fs-canonical storage, loro subscribers, notify-watcher infrastructure)
- `docs/design-plans/2026-04-19-v3-multi-agent.md` (Plan 3 — capability system for permission gating)

Can execute in parallel with Plan 3. Plan 4 (v3-extensibility) depends on the Port trait defined here — plugins register as ports.

## Acceptance Criteria

### v3-sandbox-io.AC1: LoroSyncedFile infrastructure

- **v3-sandbox-io.AC1.1 Success:** `LoroSyncedFile::open(path)` reads file content into a LoroDoc and starts a notify-watcher subscription
- **v3-sandbox-io.AC1.2 Success:** `write(content)` updates the LoroDoc and writes to disk; file content matches
- **v3-sandbox-io.AC1.3 Success:** External edit to a watched file triggers `on_external_change()` which merges via loro CRDT; both the agent's edits and the external edits are preserved
- **v3-sandbox-io.AC1.4 Success:** Self-emit-echo detection: agent write → file change → watcher fires → content hash match → no redundant merge triggered
- **v3-sandbox-io.AC1.5 Success:** `close()` drops the LoroDoc and unsubscribes the watcher; no resources leaked
- **v3-sandbox-io.AC1.6 Failure:** Opening a nonexistent file returns `FileError::NotFound(path)`
- **v3-sandbox-io.AC1.7 Edge:** Concurrent edits by agent and external process to different regions of the same file merge cleanly (both changes preserved, no data loss)
- **v3-sandbox-io.AC1.8 Edge:** Concurrent edits to the same region merge via loro CRDT semantics (last-writer-wins per character position, deterministic)

### v3-sandbox-io.AC2: File handler

- **v3-sandbox-io.AC2.1 Success:** `File.Read(path)` returns file contents without creating a LoroDoc; subsequent external edits do not generate notifications
- **v3-sandbox-io.AC2.2 Success:** `File.Open(path)` creates a LoroSyncedFile, auto-subscribes to change notifications, returns current content
- **v3-sandbox-io.AC2.3 Success:** `File.Write(path, content)` on an open file goes through loro; on an unopened file, writes directly
- **v3-sandbox-io.AC2.4 Success:** `File.Close(path)` drops LoroSyncedFile; subsequent external edits do not generate notifications
- **v3-sandbox-io.AC2.5 Success:** `File.List(path, "*.rs")` returns matching files in directory with correct metadata
- **v3-sandbox-io.AC2.6 Success:** `File.Watch(path)` subscribes to change notifications without creating a LoroDoc (lighter weight than Open)
- **v3-sandbox-io.AC2.7 Success:** External edit to an open file produces a system reminder with the diff in the agent's next turn
- **v3-sandbox-io.AC2.8 Failure:** `File.Write` to a path outside allowed directories returns `FileError::PermissionDenied` with the denied path and the applicable deny rule
- **v3-sandbox-io.AC2.9 Failure:** `File.Write` to a file that parses as pattern config KDL triggers human approval via PermissionBroker; write blocked until approved
- **v3-sandbox-io.AC2.10 Edge:** KDL config deny rule `/project/.env` blocks writes to that path even when `/project/` is in the allow list (deny evaluated first)
- **v3-sandbox-io.AC2.11 Edge:** Session serialization records open file paths; on session resume, files are re-opened with fresh LoroDoc (no LoroDoc state persisted)

### v3-sandbox-io.AC3: Shell handler

- **v3-sandbox-io.AC3.1 Success:** `Shell.Execute("echo hello", 30)` returns `{ output: "hello\n", exit_code: 0, duration_ms: ... }`
- **v3-sandbox-io.AC3.2 Success:** `Shell.Execute` auto-spawns a default shell session if none exists; subsequent executions reuse it (cwd/env preserved)
- **v3-sandbox-io.AC3.3 Success:** `Shell.Spawn("long-running-cmd")` returns a `ProcessId`; process runs asynchronously; agent receives output via system reminders
- **v3-sandbox-io.AC3.4 Success:** `Shell.Kill(pid)` terminates the process; subsequent `Shell.Status()` shows it as terminated with exit code
- **v3-sandbox-io.AC3.5 Success:** `Shell.Status()` lists all active sessions/processes with their current state
- **v3-sandbox-io.AC3.6 Success:** Shell session persists cwd across executions: `Execute("cd /tmp")` then `Execute("pwd")` returns `/tmp`
- **v3-sandbox-io.AC3.7 Failure:** `Shell.Execute` with timeout: command exceeding timeout is killed; response indicates timeout
- **v3-sandbox-io.AC3.8 Failure:** `Shell.Kill` with nonexistent `ProcessId` returns `ShellError::ProcessNotFound`
- **v3-sandbox-io.AC3.9 Edge:** Exit code detection uses OSC prompt markers (nonce-based); command output containing exit-code-like text does not confuse the parser
- **v3-sandbox-io.AC3.10 Edge:** Process output logged to file as reliability backstop; log file written even if agent session crashes

### v3-sandbox-io.AC4: Port trait and registry

- **v3-sandbox-io.AC4.1 Success:** `Port` trait defined in `pattern_core` with `id()`, `metadata()`, `subscribe()`, `call()`, `capabilities()`, `library()`
- **v3-sandbox-io.AC4.2 Success:** `PortRegistry` resolves registered ports by `PortId`; `Port.List()` returns all registered ports with metadata
- **v3-sandbox-io.AC4.3 Success:** `Port.Call(id, method, payload)` dispatches to the correct port implementation; response returned to agent
- **v3-sandbox-io.AC4.4 Success:** `Port.Subscribe(id, config)` returns a subscription; events arrive as system reminders between turns
- **v3-sandbox-io.AC4.5 Success:** `Port.Unsubscribe(id)` stops event delivery; no further system reminders from that port
- **v3-sandbox-io.AC4.6 Success:** Port with `library()` returning Haskell source: source compiled into agent's prelude when port is in CapabilitySet
- **v3-sandbox-io.AC4.7 Failure:** `Port.Call` to a port not in agent's CapabilitySet: port's effect constructors absent from prelude (compile-time rejection)
- **v3-sandbox-io.AC4.8 Failure:** `Port.Call` to an unregistered `PortId` returns `PortError::NotFound`
- **v3-sandbox-io.AC4.9 Edge:** Port library excluded from prelude when port not in CapabilitySet; agent code referencing the library fails at compilation
- **v3-sandbox-io.AC4.10 Edge:** `DataStream` trait and `SourceManager` trait removed from `pattern_core`; `cargo check --workspace` passes without them

### v3-sandbox-io.AC5: Integration and cleanup

- **v3-sandbox-io.AC5.1 Success:** `HttpPort` registered as runtime-provided port; `Port.Call("http", "get", {url})` performs HTTP request and returns response
- **v3-sandbox-io.AC5.2 Success:** System reminders from file watches, shell spawn output, and port subscriptions all appear in segment 2 of the agent's next turn
- **v3-sandbox-io.AC5.3 Success:** Smoke test at `crates/pattern_runtime/tests/sandbox_io_smoke.rs` passes deterministically: exercises shell execute, file open+write+external-edit+merge, port call+subscribe
- **v3-sandbox-io.AC5.4 Success:** Sources handler stub and Rpc handler stub deleted; only Spawn (Plan 3) and Mcp (Plan 4) stubs remain
- **v3-sandbox-io.AC5.5 Success:** `canonical_effect_decls()` updated for Shell, File, Port effects; removed Sources and Rpc declarations
- **v3-sandbox-io.AC5.6 Failure:** Any step in smoke test failing produces a clear error identifying which step and which assertion
- **v3-sandbox-io.AC5.7 Edge:** Smoke test runs concurrently with other tests without shared-state interference

## Glossary

- **PTY (pseudoterminal)**: A kernel-level terminal emulation device that allows a process to act as a terminal for another process. Used here so the shell handler can run an interactive bash session, capture its output, and detect command completion.
- **OSC prompt markers**: Escape sequences injected into the shell's `PS1` prompt (using Operating System Command ANSI escape codes) that wrap a nonce string. The shell handler looks for these markers in the PTY output stream to detect when a command has finished and what its exit code was, without ambiguity from command output.
- **LoroSyncedFile**: A Pattern-internal struct wrapping a `LoroDoc` with a filesystem path, a notify-watcher subscription, and self-emit-echo detection. Ephemeral and in-memory only — the file on disk is the authoritative persistent copy. Shared infrastructure reused by both the block subscriber system (Plan 1) and the file handler.
- **Loro / LoroDoc**: A Rust CRDT library for collaborative document editing. A `LoroDoc` is a versioned, mergeable document. Pattern uses it so that concurrent edits to the same file can be merged deterministically.
- **CRDT (conflict-free replicated data type)**: A data structure where multiple independent writers can make changes that always merge into a consistent result without coordination.
- **Self-emit-echo detection**: When the agent writes to a file, the filesystem watcher fires a change event for that same write. Echo detection via content hash comparison suppresses the redundant event.
- **System reminder**: A structured message injected into segment 2 of the agent's context, carrying out-of-band notifications — memory block changes, file diffs, shell output, port events. Delivered between conversation turns.
- **EffectHandler / effect GADT**: In Pattern's SDK, agents express side effects as values of a GADT (generalised algebraic data type). The Rust runtime dispatches these to the corresponding `EffectHandler` implementation.
- **SdkBundle**: The HList of all registered `EffectHandler` implementations assembled at startup. Each handler occupies a fixed tag position.
- **CapabilitySet**: A set of effect categories granted to an agent by configuration (Plan 3). Absent capabilities mean the corresponding effect constructors are excluded from the agent's compiled Haskell prelude.
- **Port trait**: A `pattern_core` trait giving agents a uniform call/subscribe interface to external services. Replaces `DataStream` and `SourceManager`. Port implementations can optionally provide Haskell library source compiled into the agent's prelude.
- **PortRegistry**: Runtime-owned coordinator holding registered `Port` implementations, replacing `SourceManager`.
- **DataStream / SourceManager**: Legacy `pattern_core` traits retired by this plan in favour of `Port` and `PortRegistry`.
- **ProcessManager**: Runtime-owned coordinator managing PTY shell sessions. Shared across agent sessions.
- **FileManager**: Per-session coordinator managing open `LoroSyncedFile` instances for one agent.
- **Coordinator pattern**: The architectural principle used throughout: handlers are stateless request dispatchers; stateful infrastructure is owned by long-lived coordinators behind Arc.
- **KDL**: Configuration file format used by Pattern for persona, project, and policy configuration. Config file shape detection gates writes to files that parse as pattern config KDL.
- **notify-watcher**: The `notify` crate, a cross-platform filesystem event watcher used to detect external edits to files the agent has open.

## Architecture

### Handler dispatch model

All three handlers follow the v3 SDK pattern established by the foundation: stateless `EffectHandler<U>` implementations that receive a request enum, dispatch to runtime-owned coordinators for stateful operations, and return a wire-format `Value` via `cx.respond()`. Handlers live at `crates/pattern_runtime/src/sdk/handlers/{shell,file,port}.rs` and register into the `SdkBundle` HList at fixed tag positions.

The key architectural principle: **handlers are stateless dispatchers; coordinators own state.** PTY sessions, LoroSyncedFile instances, and port connections are managed by runtime-level coordinators that outlive individual effect invocations.

### Shell handler + ProcessManager

`ShellHandler` dispatches to a `ProcessManager` coordinator, owned by the runtime (one per runtime instance, shared across sessions via Arc).

**ProcessManager** manages a map of active shell sessions (`HashMap<ProcessId, ShellSession>`). Each `ShellSession` wraps a PTY backend (ported from v2's `LocalPtyBackend`): a persistent bash process with cwd/env state, OSC prompt markers for exit-code detection, stdout/stderr streaming via a background tokio task.

**Effect surface:**
- `Shell.Execute(cmd, timeout)` — run command in the default (or specified) session, block until completion, return stdout + exit code. auto-spawns a session if none exists.
- `Shell.Spawn(cmd)` — start a long-running process, return a `ProcessId`. output streams asynchronously; agent sees it via system reminders or explicit polling.
- `Shell.Kill(pid)` — signal the process, clean up resources.
- `Shell.Status()` — list active sessions/processes with their state.

**Output delivery:** `Execute` returns output directly in the effect response. `Spawn` output accumulates and the agent receives it via system reminders (same notification channel as file watches and block changes). Process output is also written to a log file as a reliability backstop (not agent-visible, just for debugging/recovery). Piping output to blocks, ports, or files is agent-level composition in Haskell, not a runtime concern.

**Permissions:** Shell effect category in CapabilitySet (Plan 3 layer 1). Destructive command gating (e.g., `rm -rf`, `sudo`) as Rust-default policy rules (Plan 3 layer 2). Per-session — once a shell session exists, individual commands are gated by policy at dispatch time.

### File handler + FileManager

`FileHandler` dispatches to a `FileManager` coordinator, owned per-session (each agent session has its own FileManager tracking which files that agent has open).

**FileManager** manages a map of open files (`HashMap<PathBuf, LoroSyncedFile>`).

**LoroSyncedFile** — shared infrastructure extracted from Plan 1's subscriber code:
- LoroDoc (in-memory only, ephemeral — no jj, no sqlite, no block metadata)
- notify-watcher subscription for external edit detection
- Self-emit-echo detection (content hash comparison prevents write-notify-rewrite loops)
- On external edit: watcher fires → loro CRDT merge → diff computed → system reminder injected into agent's next turn
- On agent edit: LoroDoc updated → file written to disk → self-emit suppressed
- On close/session end: LoroDoc dropped, watcher unsubscribed. file on disk is the only survivor.

The `LoroSyncedFile` primitives (LoroDoc creation, merge logic, echo detection) are extracted into shared utilities at `crates/pattern_memory/src/loro_sync.rs` (or similar), reusable by both Plan 1's block subscriber system and this FileManager.

**Effect surface:**
- `File.Read(path)` — read file contents. no LoroDoc created.
- `File.Write(path, content)` — if file is open, goes through loro for CRDT merge. if not, direct write. permission-checked either way.
- `File.Open(path)` — creates LoroSyncedFile, starts watching, auto-subscribes to change notifications. returns current content.
- `File.Close(path)` — drops LoroSyncedFile, stops watching.
- `File.List(path, pattern)` — directory listing with glob filtering.
- `File.Watch(path)` — subscribe to change notifications without opening for editing (no LoroDoc, just watcher + system reminders on change).

**Permissions:** File effect category in CapabilitySet. Directory scoping via allow/deny rules in KDL config (deny evaluated first). Config file shape-based write gating (Plan 3 — any file parsing as pattern config KDL triggers human approval).

**Session state:** which files were open (paths only) serialized as part of session state for resume.

### Port effect + PortRegistry

`PortHandler` dispatches to a `PortRegistry`, owned by the runtime (shared across sessions).

**Port trait** (lives in `pattern_core`, replacing `DataStream`):

```rust
#[async_trait]
pub trait Port: Send + Sync {
    fn id(&self) -> &PortId;
    fn metadata(&self) -> PortMetadata;
    async fn subscribe(&self, config: serde_json::Value)
        -> Result<BoxStream<'static, PortEvent>, PortError>;
    async fn call(&self, method: &str, payload: serde_json::Value)
        -> Result<serde_json::Value, PortError>;
    fn capabilities(&self) -> PortCapabilities;
    fn library(&self) -> Option<&'static str> { None }
    fn as_any(&self) -> &dyn Any;
}
```

**PortRegistry** replaces `SourceManager`. Holds registered port implementations. Runtime-provided ports (e.g., `http`) register at startup. Plugin-registered ports register at plugin load time (Plan 4).

**Effect surface:**
- `Port.List()` — enumerate available ports with metadata.
- `Port.Call(id, method, payload)` — one-shot request/response.
- `Port.Subscribe(id, config)` — subscribe to event stream. events arrive as system reminders between turns.
- `Port.Unsubscribe(id)` — stop subscription.

**Library integration:** when a port is in the agent's CapabilitySet, its `library()` Haskell source is compiled into the prelude alongside effect GADTs. Port not in capability set → library excluded. This gives agents typed ergonomic access to port APIs without manually constructing JSON payloads.

**Configuration:** convention-based — `call("configure", config)`. no separate trait method.

**Replaces:** Sources handler stub + Rpc handler stub → single Port handler. `DataStream` trait → `Port` trait. `SourceManager` → `PortRegistry`.

## Existing patterns

**Handler dispatch.** All existing v3 handlers (`TimeHandler`, `LogHandler`, `MemoryHandler`, etc.) at `crates/pattern_runtime/src/sdk/handlers/` follow the same pattern: stateless `EffectHandler<U>` with request enum + `cx.respond()`. The new handlers follow this exactly.

**Coordinator pattern.** The design introduces runtime-owned coordinators (ProcessManager, FileManager, PortRegistry) that handlers delegate into. This mirrors the mailbox coordinator pattern from Plan 3 — long-lived infrastructure owned by the runtime, accessed by stateless handlers via Arc.

**v2 reference code.** ProcessSource (`rewrite-staging/runtime_subsystems/data_source/process/`), FileSource (`rewrite-staging/runtime_subsystems/data_source/file_source.rs`), ShellTool (`rewrite-staging/runtime_subsystems/tool/builtin/shell.rs`), and FileTool (`rewrite-staging/runtime_subsystems/tool/builtin/file.rs`) provide the PTY backend, CRDT wrapping, and file lifecycle implementations. The v3 design ports the mechanics (PTY management, LoroDoc lifecycle, watcher integration) while replacing the dispatch model (tools → effects, DataStream → Port).

**Plan 1's subscriber infrastructure.** The loro subscriber system (LoroDoc + notify-watcher + self-emit-echo detection + reconciliation) built in Plan 1 for block storage is the template for LoroSyncedFile. Shared utilities are extracted rather than duplicated.

**System reminders as notification channel.** Plan 1 established system reminders in segment 2 for memory block changes. This design reuses the same channel for file watch diffs and shell output — unified notification model.

## Implementation phases

<!-- START_PHASE_1 -->
### Phase 1: LoroSyncedFile shared infrastructure

**Goal:** Extract reusable loro+watcher sync primitives from Plan 1's subscriber code.

**Components:**
- `LoroSyncedFile` type at `crates/pattern_memory/src/loro_sync.rs` — LoroDoc + file path + notify-watcher subscription + self-emit-echo detection + merge logic
- Constructor: `LoroSyncedFile::open(path)` — reads file, creates LoroDoc, starts watching
- Methods: `write(content)`, `on_external_change()` → diff, `close()` → drop everything
- Refactor Plan 1's block subscriber to use the same primitives internally (if Plan 1 is complete; otherwise design for compatibility)

**Dependencies:** Plan 1 complete or in progress (loro + notify infrastructure exists).

**Done when:** `LoroSyncedFile` can open a file, track edits via loro, detect external changes, compute diffs, and handle self-emit-echo suppression. Unit tests cover concurrent edit merge, external edit detection, and echo suppression.
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: File handler + FileManager

**Goal:** Fully functional `ctx.file.*` effect surface.

**Components:**
- `FileHandler` at `crates/pattern_runtime/src/sdk/handlers/file.rs` — replaces stub
- `FileManager` at `crates/pattern_runtime/src/file_manager.rs` — per-session coordinator owning `HashMap<PathBuf, LoroSyncedFile>`
- Effect request enum: `FileReq::Read`, `Write`, `Open`, `Close`, `List`, `Watch`
- System reminder injection for file watch diffs — integration with turn composition
- Directory scoping: `FilePolicy` type with allow/deny rules, evaluated at dispatch
- Session state serialization of open file paths

**Dependencies:** Phase 1 (LoroSyncedFile). Plan 3 for full capability gating (can stub capability checks initially and wire in when Plan 3 lands).

**Done when:** Agent can open, read, write, close, list, and watch files. CRDT merge handles concurrent edits. External edits surface as system reminders. Directory allow/deny rules enforced. Tests cover: basic CRUD, concurrent edit merge, external edit notification, permission denial, config file shape detection.
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: Shell handler + ProcessManager

**Goal:** Fully functional `ctx.shell.*` effect surface.

**Components:**
- `ShellHandler` at `crates/pattern_runtime/src/sdk/handlers/shell.rs` — replaces stub
- `ProcessManager` at `crates/pattern_runtime/src/process_manager.rs` — runtime-owned coordinator managing PTY sessions
- `ShellSession` wrapping PTY backend (ported from v2's `LocalPtyBackend`) — persistent bash, OSC prompt markers, exit-code detection
- Effect request enum: `ShellReq::Execute`, `Spawn`, `Kill`, `Status`
- System reminder injection for async spawn output
- Process output logging to file (reliability backstop)

**Dependencies:** None beyond existing runtime infrastructure. Can be built in parallel with Phase 2.

**Done when:** Agent can execute commands (sync), spawn processes (async), kill them, and query status. PTY session persists cwd/env across executions. Async output surfaces as system reminders. Tests cover: execute with exit code, spawn + kill lifecycle, timeout enforcement, concurrent session management. PTY tests use temp PTY with shell builtins only (CI-safe).
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: Port trait and PortRegistry

**Goal:** `Port` trait in pattern_core, `PortRegistry` replacing `SourceManager`, `PortHandler` replacing Sources + Rpc stubs.

**Components:**
- `Port` trait at `crates/pattern_core/src/traits/port.rs` — id, metadata, subscribe, call, capabilities, library
- `PortId`, `PortMetadata`, `PortCapabilities`, `PortEvent`, `PortError` types at `crates/pattern_core/src/types/port.rs`
- `PortRegistry` at `crates/pattern_runtime/src/port_registry.rs` — replaces `SourceManager`
- `PortHandler` at `crates/pattern_runtime/src/sdk/handlers/port.rs` — replaces Sources + Rpc stubs
- Effect request enum: `PortReq::List`, `Call`, `Subscribe`, `Unsubscribe`
- Library integration: port's `library()` source compiled into prelude when port is in CapabilitySet
- Retire `DataStream` trait and `SourceManager` trait from `pattern_core`
- Delete Sources and Rpc handler stubs

**Dependencies:** None beyond existing runtime. Port trait is standalone.

**Done when:** Port trait defined and exported. PortRegistry holds and resolves ports. PortHandler dispatches all four operations. Library compilation into prelude works. DataStream and SourceManager traits removed. Sources and Rpc stubs deleted. Tests cover: port registration + discovery, one-shot call, subscribe/unsubscribe lifecycle, library prelude injection, capability filtering.
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: Runtime-provided ports and integration

**Goal:** Ship built-in ports, wire system reminders for all notification sources, end-to-end smoke test.

**Components:**
- `HttpPort` — runtime-provided port for one-shot HTTP requests (`call("get", url)`, `call("post", {url, body})`)
- System reminder unification — file watch diffs, shell spawn output, and port subscribe events all flow through the same system reminder injection mechanism in turn composition
- End-to-end smoke test at `crates/pattern_runtime/tests/sandbox_io_smoke.rs` — exercises shell execute + spawn, file open + write + external edit + merge, port call + subscribe, capability enforcement
- Cleanup: update handler registration in SdkBundle, update canonical_effect_decls() for new/removed effects

**Dependencies:** Phases 1-4.

**Done when:** HttpPort functional. All notification sources (file, shell, port) deliver via system reminders. Smoke test exercises the full sandbox I/O surface deterministically (mock provider, mock port, temp PTY, temp files). No stub handlers remain except Spawn (Plan 3) and Mcp (Plan 4).
<!-- END_PHASE_5 -->

## Execution mode recommendation

**Collaborative.** The LoroSyncedFile extraction and Port trait design involve novel shared infrastructure where getting the abstraction boundaries right matters. The shell handler is more mechanical (v2 reference is solid) but the file handler's CRDT integration and the Port trait's library capability are novel enough to benefit from human check-in points. 5 phases, moderate complexity — collaborative fits well.

## Additional considerations

**Process output logging.** Shell command output is written to a log file as a reliability backstop (similar to claude code's approach). This is runtime-internal, not agent-visible. The log location should be configurable and follow the same rotation policy as Plan 1's message backup.

**Timer stays in Time module.** Periodic and one-shot timers are part of `ctx.time.*`, not ports. Timer is a fundamental runtime capability, not an external service interaction.
