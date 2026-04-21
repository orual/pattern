# Pattern v3 Extensibility Design

## Summary

Pattern's extensibility plan builds a unified plugin system where every plugin — regardless of origin — is managed through a single `PluginExtension` trait. Native IRPC plugins implement the trait directly via a `pattern-plugin-sdk` crate. Claude Code plugins are wrapped by a `CcPluginAdapter` that translates CC conventions (skills, agents, hooks, monitors, MCP servers) into pattern semantics, running in-process via IRPC's zero-overhead tokio channel mode. Standalone MCP servers are wrapped by a `McpPluginAdapter`. The runtime manages all three uniformly: one plugin lifecycle, one hook dispatch path, one port registration surface. Plugins that need richer integration (memory access, message sending, task creation) use the `PluginHost` callback interface available over IRPC's bidirectional channel; CC and MCP adapters stub these methods gracefully.

Alongside the plugin system, this plan restructures how Pattern interacts with MCP servers as a client. Rather than registering every MCP tool into a flat tool registry, the runtime injects a concise server overview as a system reminder and materialises full tool documentation as searchable memory blocks, letting agents load details on demand. Plugins register ports (the external service abstraction from v3-sandbox-io) as their primary integration surface — agents interact with plugin capabilities through `ctx.port.*`. All plugin-installed components are subject to the capability-based permission system from Plan 3, and plugin-installed skills receive the `PluginInstalled` trust tier reserved in Plan 2. The existing `pattern_mcp` crate is dissolved into `pattern_runtime`, and its unimplemented server stub is removed.

## Definition of Done

Plan 4 in the Pattern v3 rewrite sequence. Builds the plugin system, MCP inverted surface, iroh-rpc transport, and hook lifecycle on top of Plans 1-3. Consumes Plan 3's capability-based permission system for trust enforcement. The plan is done when:

### Plugin interface

- **One trait, all plugins:** `PluginExtension` — every plugin implements this, directly or via adapter. Lifecycle callbacks (`on_install`, `on_enable`, `on_disable`), port declarations (`ports()`), event handling (`on_event()`), optional Haskell library (`library()`).
- **`PluginHost`** — pattern's callback surface for plugins that need to reach into the runtime (memory access, message sending, task creation). IRPC-native plugins get the real implementation. CC adapter stubs with clear `NotSupported` errors.
- **Three adapters, one runtime code path:**
  - Native IRPC plugins implement `PluginExtension` directly via `pattern-plugin-sdk` crate
  - `CcPluginAdapter` wraps CC plugin directories — translates skills/agents/hooks/monitors/MCP into pattern semantics, runs in-process via IRPC in-process mode (tokio mpsc, zero network overhead)
  - `McpPluginAdapter` wraps standalone MCP servers as `PluginExtension` implementations
- Transport is an implementation detail. The runtime manages all plugins uniformly through `PluginExtension`.

### Plugin manifest and loader

- Pattern-native plugin manifest in KDL format. CC-compat `plugin.json` also parseable — both normalize to `PluginManifest` internally.
- CC artifact translation (handled by `CcPluginAdapter`):
  - CC skills → pattern Skill blocks (trust-tagged `PluginInstalled`)
  - CC agents → pattern agent spawn configs (default `spawn_mode: ephemeral`; opt-in to persona via `pattern.persona_mode`)
  - CC hooks → `on_event()` dispatch via CC event alias table
  - CC monitors → Port implementations (stdout stream → `Port.subscribe` semantics)
  - CC MCP servers → MCP connections (stay as MCP or wrap as ports)
  - CC commands → pattern utilities with audience tier
  - CC `bin/` → executables added to shell PATH
- Unknown fields silently ignored (bidirectional non-breaking with stock CC hosts).
- Plugin directory layout: `~/.pattern/plugins/<plugin-id>/` (global), `<project>/.pattern/shared/plugins/<plugin-id>/` (project-scoped committed), `<project>/.pattern/private/plugins/<plugin-id>/` (project-scoped private, gitignored).
- Plugins cached in `~/.pattern/plugins/cache/` (cloned on install, updated on schedule).
- Load precedence: project > global > ambient. Collisions warned at install.

### MCP inverted surface (client-only)

- MCP client folded into `pattern_runtime` (no trait in `pattern_core`, no separate `pattern_mcp` crate). Tested via stdio transport against mock MCP servers.
- Three levels of MCP awareness:
  1. **System reminder in segment 2** — concise MCP server overview (server name + one-line per tool) injected as a system reminder pseudo-message. Dynamic load/unload without busting segment 1 cache.
  2. **Detail docs as Working-tier blocks** — full tool documentation at `mcp/<server>/<tool>.md`. Searchable via FTS5+vector. Agent loads on demand via `ctx.skills.load`.
  3. **Single dispatch primitive** — `ctx.mcp.call(server, method, args)`. Discovery via `ctx.mcp.introspect(server)`, `ctx.mcp.list_servers()`.
- Per-server scoped permissions integrated with Plan 3's capability system.
- MCP server stub removed from the codebase.

### IRPC transport

- IRPC (n0-computer's streaming RPC for iroh) as the native plugin transport. Built on `irpc` crate over QUIC.
- `PluginExtension` trait serialised over IRPC — runtime calls `ports()`, `on_event()`, etc. via RPC. Plugin calls back via `PluginHost` over the same bidirectional channel.
- In-process mode (IRPC over tokio mpsc) used by `CcPluginAdapter` and `McpPluginAdapter` — zero network overhead for wrapped plugins.
- Out-of-process mode (IRPC over QUIC) for native IRPC plugins, both local (loopback) and remote.
- Plugin authors use `pattern-plugin-sdk` Rust crate: implement `PluginExtension`, get IRPC wiring + typed `PluginHost` client handle for free.
- Plugin manifest declares transport preference via `pattern.transport` field.
- One plugin can expose multiple ports (e.g., one for tool calls, one for event streaming).

### Hook lifecycle system

- Comprehensive event taxonomy — everything hookable from a programmer's perspective. CC plugins use a mapped subset.
- Per-event sync/async semantics (event type determines it, not hook config):
  - **Blocking events** (hook completes before event proceeds): `turn.before`, `turn.after`, `tool.before`, `tool.after`, `permission.request`
  - **Notification events** (fire-and-forget): `memory.write`, `memory.read`, `fork.spawn`, `fork.resolve`, `compaction.cycle.start`, `compaction.cycle.end`, `persona.attach`, `persona.detach`, `plugin.install`, `plugin.uninstall`, `spawn.ephemeral`, `spawn.sibling`, `wake.condition.fired`
- CC event aliases mapped: `SessionStart` → `persona.attach`, `UserPromptSubmit` → `turn.before`, `PreToolUse`/`PostToolUse` → `tool.before`/`tool.after`, etc. CC plugins register by CC event name; pattern translates at load time.
- Hooks fire in standard pathway; cannot bypass runtime capability gates.

### Trust enforcement

- Plugin-installed skills assigned `PluginInstalled` trust tier (the value reserved in Plan 2, now with an actual code path).
- Plugin effects respect Plan 3's capability system — plugin capabilities are scoped per plugin manifest declarations + user override per-plugin.
- Ad-hoc skills (non-plugin source) follow body-redact + user-enable flow on first use.
- Plugin MCP servers get same MCP permissions model as CC.

### Plugin capabilities

- Plugins can register: ports, agents, skills, commands, hooks, MCP servers, MessageRouter endpoints, Haskell libraries.
- All registration via `PluginExtension` trait methods, resolved at load time.
- Ports registered via `ports()`. Hooks via `on_event()`. Libraries via `library()`.
- CC-compat plugins have their artifacts translated by `CcPluginAdapter` into the same registrations.

### Plugin transports

All plugins present as `PluginExtension` implementations regardless of transport:

- **IRPC in-process** (tokio mpsc) — used by `CcPluginAdapter` and `McpPluginAdapter`. Zero overhead. The adapter struct implements the trait directly.
- **IRPC out-of-process** (QUIC over loopback) — native IRPC plugins running as separate processes on the same machine.
- **IRPC remote** (QUIC over network) — native IRPC plugins on different machines. Requires atproto-backed mutual authentication.
- **WASM**: explicitly out of scope (v2+).

### Testing

- Plugin loader integration tests (parse manifest, translate to pattern primitives, handle unknown fields gracefully)
- MCP inverted surface tests (tool doc materialization, search, `ctx.mcp.call` dispatch)
- iroh-rpc transport tests (local QUIC communication, plugin auth)
- Hook lifecycle tests (events fire at correct points, CC alias mapping, hooks respect capability gates)
- Trust enforcement tests (plugin-installed skill tier assignment, capability scoping)
- No live-model dependency in CI paths

### Explicitly OUT OF SCOPE (deferred)

- WASM component model transport (v2+)
- Plugin marketplace / discovery
- Cross-device constellation coordination (iroh enables it but coordination is separate work)
- MCP server (Pattern exposing tools to other MCP clients)
- Social plugins (pattern-atproto, pattern-discord) — separate implementation efforts consuming this plugin system

### Context

This is the fifth design plan in the Pattern v3 rewrite sequence. Builds on:

- `docs/design-plans/2026-04-16-v3-foundation.md` (foundation)
- `docs/design-plans/2026-04-19-v3-memory-rework.md` (Plan 1 — memory)
- `docs/design-plans/2026-04-19-v3-task-skill-blocks.md` (Plan 2 — tasks + skills, reserves PluginInstalled trust tier)
- `docs/design-plans/2026-04-19-v3-multi-agent.md` (Plan 3 — subagents, coordination, capability-based permissions)
- `docs/design-plans/2026-04-19-v3-sandbox-io.md` (v3-sandbox-io — Port trait, shell/file handlers)
- `docs/plans/2026-04-16-rewrite-v3-design-draft.md` §5 (plugin layer brainstorm)

## Acceptance Criteria

### v3-extensibility.AC1: Plugin manifest parsing

- **v3-extensibility.AC1.1 Success:** KDL-format plugin manifest parses to `PluginManifest` with all declared fields (name, skills, agents, commands, hooks, transport, declared_effects)
- **v3-extensibility.AC1.2 Success:** CC-format JSON `plugin.json` parses to the same `PluginManifest` type; normalized representation matches equivalent KDL manifest
- **v3-extensibility.AC1.3 Success:** Unknown fields in both KDL and JSON manifests are silently ignored; parsing succeeds
- **v3-extensibility.AC1.4 Failure:** Manifest missing required `name` field produces `ManifestError::MissingField("name")` with file path
- **v3-extensibility.AC1.5 Edge:** Manifest with only `name` and no components parses successfully (empty plugin, valid for testing/scaffolding)

### v3-extensibility.AC2: Plugin registry and lifecycle

- **v3-extensibility.AC2.1 Success:** Plugin install clones to `~/.pattern/plugins/cache/<plugin-id>/`; registry records the installation; persisted KDL config written
- **v3-extensibility.AC2.2 Success:** After runtime restart, registry loads from persisted KDL; all previously installed plugins re-registered with their config tunables
- **v3-extensibility.AC2.3 Success:** Plugin uninstall removes from registry and cache; `plugin.uninstall` hook event fires
- **v3-extensibility.AC2.4 Success:** Load precedence: project-scoped plugin overrides global plugin with same ID; warning logged about the override
- **v3-extensibility.AC2.5 Failure:** Installing a plugin with a collision (same ID at same scope) produces `RegistryError::Collision` with both locations
- **v3-extensibility.AC2.6 Edge:** Plugin config tunables editable in persisted KDL between restarts; changes take effect on next load

### v3-extensibility.AC3: CC plugin adapter

- **v3-extensibility.AC3.1 Success:** CC-format plugin wrapped by `CcPluginAdapter`; adapter implements `PluginExtension`; runtime manages it identically to native plugins
- **v3-extensibility.AC3.2 Success:** CC plugin's skills translated to Skill blocks with `trust_tier: PluginInstalled`; visible via `ctx.skills.list()`
- **v3-extensibility.AC3.3 Success:** CC plugin's agents translated to spawn configs; invokable via the plugin's declared interface
- **v3-extensibility.AC3.4 Success:** CC plugin's monitors translated to Port implementations; subscribable via `ctx.port.subscribe()`
- **v3-extensibility.AC3.5 Success:** CC plugin's hooks dispatch through `on_event()` with CC event alias mapping (e.g., `PreToolUse` → `tool.before`)
- **v3-extensibility.AC3.6 Success:** CC compatibility Haskell library included in agent prelude; maps CC terminology to pattern terminology
- **v3-extensibility.AC3.7 Failure:** CC adapter's `PluginHost` methods return `PluginError::NotSupported` with clear message explaining CC plugins don't support host callbacks
- **v3-extensibility.AC3.8 Failure:** CC plugin subprocess crashes; `PluginError::ProcessDied` surfaced; plugin marked unhealthy in registry

### v3-extensibility.AC4: Hook lifecycle

- **v3-extensibility.AC4.1 Success:** `turn.before` hook fires before turn processing begins; hook can return a modification (e.g., prepend content) that affects the turn
- **v3-extensibility.AC4.2 Success:** `tool.before` hook fires before tool dispatch; hook can return `HookResponse::Block` to prevent tool execution
- **v3-extensibility.AC4.3 Success:** `memory.write` hook fires after a memory write completes; hook receives block handle and change summary; return value ignored (notification)
- **v3-extensibility.AC4.4 Success:** CC alias mapping: hook registered as `PreToolUse` fires on `tool.before` events; hook registered as `SessionStart` fires on `persona.attach`
- **v3-extensibility.AC4.5 Failure:** Hook execution exceeds timeout; hook treated as returning no response; event proceeds; warning logged
- **v3-extensibility.AC4.6 Failure:** Blocking hook attempts to call an effect not in the runtime's capability set; hook's effect denied (hooks respect capability gates)
- **v3-extensibility.AC4.7 Edge:** Multiple hooks registered for the same event fire in registration order; all complete before event proceeds (blocking) or all fire independently (notification)

### v3-extensibility.AC5: MCP inverted surface

- **v3-extensibility.AC5.1 Success:** On MCP server load, system reminder pseudo-message injected into segment 2 containing server name + one-line per tool
- **v3-extensibility.AC5.2 Success:** On MCP server load, Working-tier blocks created at `mcp/<server>/<tool>.md` with full tool documentation; searchable via `ctx.memory.search`
- **v3-extensibility.AC5.3 Success:** `ctx.mcp.call(server, method, args)` dispatches to the correct MCP server via rmcp; response returned to agent
- **v3-extensibility.AC5.4 Success:** `ctx.mcp.introspect(server)` returns structured tool metadata (name, description, input schema summary) for all tools on the server
- **v3-extensibility.AC5.5 Success:** `ctx.mcp.list_servers()` returns all loaded MCP servers with connection status
- **v3-extensibility.AC5.6 Success:** MCP server unload removes system reminder from subsequent turns and deletes tool doc blocks
- **v3-extensibility.AC5.7 Failure:** `ctx.mcp.call` to a server not in the agent's CapabilitySet returns `CapabilityError::Denied`
- **v3-extensibility.AC5.8 Failure:** `ctx.mcp.call` to a disconnected server returns `McpError::ServerUnavailable` with reconnection hint
- **v3-extensibility.AC5.9 Edge:** MCP server load/unload does not invalidate segment 1 cache (system prompt unchanged; only segment 2 system reminders change)
- **v3-extensibility.AC5.10 Edge:** MCP server stub deleted from codebase; `cargo check --workspace` passes without `pattern_mcp` in members list

### v3-extensibility.AC6: IRPC transport and plugin SDK

- **v3-extensibility.AC6.1 Success:** IRPC-native plugin registers ports via `ports()` over IRPC; pattern records the plugin's declared ports and capabilities
- **v3-extensibility.AC6.2 Success:** Agent calls `ctx.port.call(plugin_port, method, payload)`; dispatched to plugin's port implementation over IRPC; response returned
- **v3-extensibility.AC6.3 Success:** Plugin calls back to Pattern via `PluginHost` — `read_memory` returns block content, `send_message` delivers to target agent, `create_task` adds to TaskList
- **v3-extensibility.AC6.4 Success:** Agent calls `ctx.port.subscribe(plugin_port, config)`; events stream from plugin to pattern via IRPC server-stream; delivered as system reminders
- **v3-extensibility.AC6.5 Success:** `McpPluginAdapter` wraps standalone MCP server as `PluginExtension`; MCP tools accessible as port calls; MCP resources as port subscriptions
- **v3-extensibility.AC6.6 Success:** Per-plugin cryptographic auth via iroh node identity (local); atproto-backed mutual auth (remote)
- **v3-extensibility.AC6.7 Failure:** IRPC connection to plugin drops; plugin marked unhealthy; reconnection attempted; `PluginError::TransportLost` surfaced on next call
- **v3-extensibility.AC6.8 Edge:** `pattern-plugin-sdk` crate compiles with minimal dependencies; does not pull in `pattern_runtime` or `pattern_memory`
- **v3-extensibility.AC6.9 Edge:** IRPC in-process mode (tokio mpsc) used by CC and MCP adapters verifiably has zero network overhead

### v3-extensibility.AC7: Trust enforcement

- **v3-extensibility.AC7.1 Success:** Skills from installed plugins receive `trust_tier: PluginInstalled` via the code path reserved in Plan 2
- **v3-extensibility.AC7.2 Success:** Plugin capabilities scoped per manifest declaration; plugin agent cannot use effects beyond what the manifest declares
- **v3-extensibility.AC7.3 Success:** User override in KDL config can expand or restrict a plugin's declared capabilities; override takes precedence
- **v3-extensibility.AC7.4 Success:** Ad-hoc skill (non-plugin source) triggers body-redact + user-enable flow on first use
- **v3-extensibility.AC7.5 Failure:** Plugin agent attempts to use an effect not in its manifest-declared or user-overridden capabilities; rejected at prelude filtering (compile-time)
- **v3-extensibility.AC7.6 Edge:** Plugin with no declared capabilities gets an empty CapabilitySet; can only perform pure computation

### v3-extensibility.AC8: End-to-end integration

- **v3-extensibility.AC8.1 Success:** Smoke test at `crates/pattern_runtime/tests/plugin_smoke.rs` passes: installs CC-format plugin (via CcPluginAdapter), installs native IRPC plugin, wraps MCP server (via McpPluginAdapter), verifies skill trust tiers, hook events fire, MCP inverted surface works, port registration works, capability enforcement active
- **v3-extensibility.AC8.2 Success:** Mock ProviderClient and mock MCP server (stdio); no live model or network dependency in CI
- **v3-extensibility.AC8.3 Success:** `pattern_mcp` crate fully removed from workspace; all MCP client code lives in `pattern_runtime`
- **v3-extensibility.AC8.4 Failure:** Any step in the smoke flow failing produces a clear error identifying which step and which assertion
- **v3-extensibility.AC8.5 Edge:** Plugin smoke test runs concurrently with other tests without shared-state interference

## Glossary

- **KDL**: A document language used for Pattern's native plugin manifest format and configuration files. Human-readable, supports typed values and nested nodes.
- **CC / Claude Code**: Anthropic's Claude Code product. Pattern's plugin system is designed to load CC-format plugins (using CC's `plugin.json` manifest and stdin/stdout protocol) without modification, making Pattern a compatible host for the existing CC plugin ecosystem.
- **MCP (Model Context Protocol)**: A standard protocol for exposing tools and resources to LLM agents. In this plan Pattern acts only as an MCP client (connecting to external MCP servers), never as a server.
- **rmcp**: The Rust MCP SDK crate. Pattern's existing MCP client implementation is built on it.
- **IRPC**: A streaming RPC framework built by n0-computer on top of iroh's QUIC transport. Supports four interaction patterns: unary RPC, client streaming, server streaming, and bidirectional streaming.
- **iroh**: A peer-to-peer networking library from n0-computer that provides QUIC-based connectivity with cryptographic node identities. IRPC is built on top of it.
- **QUIC**: A UDP-based transport protocol with built-in encryption and multiplexing. Used here via iroh for local inter-process communication between Pattern and plugins.
- **ALPN (application-layer protocol negotiation)**: A TLS extension that lets two peers agree on which protocol to use during the handshake. Used here for versioning the plugin protocol (`pattern-plugin/1`).
- **postcard**: A compact binary serialization format used by IRPC for message encoding.
- **PluginExtension**: The Rust trait every plugin implements — directly (IRPC-native) or via adapter (`CcPluginAdapter`, `McpPluginAdapter`). Defines port declarations, lifecycle callbacks, event handling, and optional Haskell library provision. The runtime manages all plugins uniformly through this trait.
- **PluginHost**: The Rust trait Pattern implements for plugin callbacks — memory access, message sending, task creation. Available to IRPC-native plugins via the bidirectional channel. CC and MCP adapters stub all methods with `NotSupported` errors.
- **CcPluginAdapter**: An in-process `PluginExtension` implementation that wraps a Claude Code plugin directory, translating CC conventions into pattern semantics. Includes a CC compatibility Haskell library for terminology mapping.
- **McpPluginAdapter**: An in-process `PluginExtension` implementation that wraps a standalone MCP server, translating MCP tools to port calls and MCP resources to port subscriptions.
- **PluginInstalled trust tier**: A trust level in Pattern's skill permission system, reserved in Plan 2 and first assigned with real logic in this plan. Skills from installed plugins receive this tier rather than the higher trust given to built-in or user-authored skills.
- **CapabilitySet**: The set of runtime effects that a given agent or plugin is permitted to use, defined in Plan 3's capability-based permission system.
- **Working-tier blocks**: A memory block tier in Pattern's memory system (defined in Plan 1). Blocks at this tier are active working context — readable and searchable by agents during a session. MCP tool documentation is materialised here.
- **FTS5**: SQLite's fifth-generation full-text search extension, used by `pattern_db` for text search over memory blocks.
- **Port trait**: The external service abstraction defined in v3-sandbox-io. Plugins register ports via `PluginExtension::ports()`. Agents interact with plugin capabilities through `ctx.port.*`. Replaces the retired `DataStream` trait.
- **MessageRouter**: Pattern's internal routing layer for messages between agents and external endpoints. Plugins can register their own endpoints into this system at load time.
- **Segment 1 / segment 2**: Divisions of an agent's context window. Segment 1 is the stable system prompt (expensive to change because it busts the LLM provider's prompt cache). Segment 2 is the historical message stream, a lower-cost area for dynamic injections like MCP server reminders.
- **Persona**: Pattern's term for a persistent agent identity with associated memory and configuration. Distinct from an ephemeral spawned agent.
- **IRPC in-process mode**: IRPC's tokio mpsc channel transport, used by `CcPluginAdapter` and `McpPluginAdapter`. Zero network overhead — the adapter struct runs in the pattern process and calls trait methods directly through mpsc channels.
- **Pipe transport**: The stdin/stdout communication channel between Pattern and a CC plugin subprocess. Used internally by `CcPluginAdapter` for CC commands. Not a separate plugin tier — it's an implementation detail of the CC adapter.

## Architecture

### Plugin interface

Two traits define the plugin boundary:

```rust
/// Plugin implements this. Pattern calls into it.
/// All plugins implement this — directly (IRPC-native) or via adapter (CC, MCP).
pub trait PluginExtension: Send + Sync {
    fn new() -> Self where Self: Sized;
    
    // what this plugin provides
    fn ports(&self) -> Vec<PortDeclaration>;
    fn library(&self) -> Option<&'static str> { None }
    
    // lifecycle
    fn on_install(&mut self, ctx: &PluginContext) -> Result<(), PluginError> { Ok(()) }
    fn on_enable(&mut self, ctx: &PluginContext) -> Result<(), PluginError> { Ok(()) }
    fn on_disable(&mut self, ctx: &PluginContext) -> Result<(), PluginError> { Ok(()) }
    
    // runtime event handling (hooks)
    fn on_event(&mut self, event: &HookEvent) -> Option<HookResponse> { None }
}

/// Pattern runtime implements this. Plugin calls back for Pattern services.
/// Available to IRPC-native plugins. CC/MCP adapters stub with NotSupported errors.
pub trait PluginHost: Send + Sync {
    fn read_memory(&self, handle: BlockHandle) -> Result<BlockContent, PluginError>;
    fn write_memory(&self, handle: BlockHandle, content: BlockContent) -> Result<(), PluginError>;
    fn send_message(&self, to: PersonaId, content: MessageContent) -> Result<(), PluginError>;
    fn create_task(&self, block: BlockHandle, item: TaskItem) -> Result<TaskItemId, PluginError>;
    fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>, PluginError>;
}
```

**One trait, three adapters — one runtime code path:**

- **IRPC-native plugins** implement `PluginExtension` directly via `pattern-plugin-sdk`. Full bidirectional communication — `PluginHost` callbacks available over the same IRPC channel. Richest integration.
- **`CcPluginAdapter`** wraps CC plugin directories as `PluginExtension` implementations. Translates CC artifacts (skills, agents, hooks, monitors, MCP servers, `bin/`) into pattern semantics. Runs in-process via IRPC in-process mode (tokio mpsc channels). `PluginHost` methods stub with `PluginError::NotSupported` — CC plugins don't have that concept.
- **`McpPluginAdapter`** wraps standalone MCP servers as `PluginExtension` implementations. MCP tools become port `call` operations. MCP resource subscriptions become port `subscribe` operations. `PluginHost` stubs same as CC adapter.

The runtime manages all plugins uniformly through `PluginExtension`. It does not know or care about the underlying transport.

### Plugin registry and lifecycle

Central `PluginRegistry` in `pattern_runtime`:

```rust
pub struct PluginRegistry {
    plugins: HashMap<PluginId, LoadedPlugin>,
}

pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub skills: Vec<BlockHandle>,
    pub agents: Vec<AgentConfig>,
    pub hooks: Vec<HookBinding>,
    pub mcp_servers: Vec<McpServerHandle>,
    pub data_streams: Vec<StreamId>,
    pub endpoints: Vec<EndpointId>,
    pub capabilities: CapabilitySet,
    pub transport: Transport,
}
```

Registry state persisted to KDL file for config tunables and restart survival. Plugins cached in `~/.pattern/plugins/cache/` (cloned on install, updated on schedule, similar to CC's `~/.claude/plugins/cache/`).

**Plugin manifest.** Pattern-native format is KDL. CC-compat JSON (`plugin.json`) also parseable. Both normalize to `PluginManifest` internally. Pattern-native KDL manifest supports richer declarations (effect requirements, transport preference, IRPC service definition).

### MCP inverted surface

MCP client implementation folded into `pattern_runtime` from the existing `pattern_mcp` crate. No `McpClient` trait in `pattern_core` — MCP is a runtime concern. Testing via stdio transport against mock MCP servers.

Three levels of MCP awareness for agents:

1. **System reminder (segment 2).** When MCP servers load, a concise overview (server name + one-line per tool) is injected as a system reminder pseudo-message in the conversation stream. Dynamic — loading/unloading a server adds/removes a reminder without busting segment 1 cache. Agent always knows what's available.

2. **Detail docs as Working-tier blocks.** On server load, full tool documentation (parameter schemas, usage examples, edge cases) is materialized as Working-tier blocks at `mcp/<server>/<tool>.md`. Searchable via FTS5 + vector. Agent loads specific docs on demand via `ctx.skills.load("mcp/github/create_issue")` when it needs to use a tool.

3. **Single dispatch primitive.** `ctx.mcp.call(server, method, args)` is the only way agents interact with MCP servers. No tool list explosion. Discovery via `ctx.mcp.introspect(server)` (returns structured tool metadata) and `ctx.mcp.list_servers()`.

Per-server permissions integrated with Plan 3's capability system. MCP server access is part of the CapabilitySet. Individual server and tool permissions configurable in KDL config.

### Hook lifecycle

Comprehensive event taxonomy. Per-event semantics — the event type determines whether hooks block or fire-and-forget, not the hook's own configuration.

**Blocking events** (hook must complete before the triggering action proceeds): `turn.before`, `turn.after`, `tool.before`, `tool.after`, `permission.request`.

**Notification events** (fire-and-forget, no return value): `memory.write`, `memory.read`, `fork.spawn`, `fork.resolve`, `compaction.cycle.start`, `compaction.cycle.end`, `persona.attach`, `persona.detach`, `plugin.install`, `plugin.uninstall`, `spawn.ephemeral`, `spawn.sibling`, `wake.condition.fired`.

CC event aliases: `SessionStart` → `persona.attach`, `UserPromptSubmit` → `turn.before`, `PreToolUse`/`PostToolUse` → `tool.before`/`tool.after`, `SessionEnd` → `persona.detach`. CC plugins register by CC event name; pattern translates at plugin load time.

Hooks fire through the standard capability pathway — a hook cannot bypass the runtime's permission gates.

### IRPC integration

Built on the `irpc` crate (n0-computer's streaming RPC for iroh, v0.13+). IRPC provides four interaction patterns (standard RPC, client streaming, server streaming, bidirectional streaming) over QUIC with postcard serialization.

`PluginExtension` trait methods are serialised over IRPC for out-of-process plugins:

- `ports()` → plugin declares available ports
- `on_event(HookEvent)` → pattern sends lifecycle events
- `library()` → plugin provides Haskell helper source
- Port `call`/`subscribe` operations dispatched to the plugin's port implementations over the same IRPC channel

Plugin-side callbacks to pattern (via `PluginHost`):

- `read_memory`, `write_memory`, `send_message`, `create_task`, `search`

For in-process adapters (`CcPluginAdapter`, `McpPluginAdapter`), the same trait methods are called directly without serialisation — IRPC in-process mode uses tokio mpsc channels.

QUIC over loopback for local communication. ALPN protocol negotiation enables versioning (`pattern-plugin/1`).

**Authentication model (tiered):**

| Scenario | Auth mechanism |
|---|---|
| Local IRPC, plugin doesn't opt in | iroh node identity only (same-machine trust boundary) |
| Local IRPC, plugin opts into atproto auth | atproto record pair (same DID, same repo) |
| Remote IRPC, same user both sides | atproto record pair (same DID, same repo) |
| Remote IRPC, different operators | atproto record pair (different DIDs, cross-repo) |

**Local (default):** iroh node identity is sufficient — each plugin gets a unique key pair, the runtime knows which key belongs to which plugin (registered at install time), and rejects unknown keys.

**Remote / opted-in:** atproto-backed mutual authentication, inspired by weaver.sh's iroh-gossip collaboration pairing. On initial pairing, both sides (runtime and plugin) publish an atproto record referencing the other's DID and iroh node key, signed over the record content in dag-cbor canonical form. On subsequent connections, both sides resolve the counterpart's record URI, validate the signature, and verify that the record references their own DID and node key. Revocation = record deletion — next connection attempt fails validation. This works identically for same-user (both records in same repo) and cross-user (records in different repos) scenarios. The plugin's atproto DID can be the user's own DID (most common for personal plugins), the plugin author's DID (for hosted multi-tenant services), or a per-installation identity.

`pattern-plugin-sdk` crate provides: IRPC server scaffold, typed `PluginHost` client handle, trait implementations for `PluginExtension`. Plugin authors implement a trait and the SDK handles serialization, transport, and registration.

## Existing patterns

**MCP client.** The existing MCP client implementation in `crates/pattern_mcp/src/` (rmcp-based, stdio/HTTP/SSE transports) is folded into `pattern_runtime`. The `ToolRegistryBridge` pattern that converts MCP tools to pattern's `ToolRegistry` format is replaced by the inverted surface — tools are no longer registered, they're dispatched through `ctx.mcp.call`.

**MCP server.** The existing MCP server stub (`McpServerBuilder` with `todo!` start/stop) in `pattern_mcp` is deleted. Not in scope.

**MessageRouter and endpoints.** The existing endpoint registration pattern (`EndpointRegistry` trait, dynamic endpoint registration) is reused for plugin-registered MessageRouter endpoints. Plugins register endpoints at load time via the same mechanism.

**Port trait (from v3-sandbox-io).** The `Port` trait defined in the sandbox-io plan replaces `DataStream`. Plugins register ports via `PluginExtension::ports()`. Plugin-provided ports are the primary integration surface — agents interact with plugins through `ctx.port.*`.

**CC plugin format.** CC plugins use a directory-based convention (skills/, agents/, hooks/, monitors/, .mcp.json, bin/) without a runtime trait. The `CcPluginAdapter` translates these conventions into `PluginExtension` semantics. CC documentation references CC terminology — the adapter includes a Haskell compatibility library that maps CC names to pattern names where they differ.

## Implementation phases

<!-- START_PHASE_1 -->
### Phase 1: Plugin manifest and registry

**Goal:** KDL plugin manifest parsing, CC JSON compat, PluginRegistry with persistence.

**Components:**
- `PluginManifest` type and KDL parser in `crates/pattern_runtime/src/plugin/manifest.rs` — native KDL format
- CC JSON manifest parser — reads `plugin.json`, normalizes to `PluginManifest`
- `PluginRegistry` in `crates/pattern_runtime/src/plugin/registry.rs` — HashMap of loaded plugins, persistence to KDL file
- Plugin cache directory management — `~/.pattern/plugins/cache/`, clone on install, update checks
- Plugin directory layout and load precedence (project > global > ambient)
- `PluginId` type, install/uninstall operations

**Dependencies:** Plan 3 complete (CapabilitySet for per-plugin capability declarations).

**Done when:** KDL and JSON manifests parse correctly. Registry persists across restarts. Plugin install clones to cache. Load precedence resolves correctly. Collision detection warns.
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: Plugin trait boundary and CC adapter

**Goal:** `PluginExtension`/`PluginHost` traits, `CcPluginAdapter` wrapping CC plugin directories.

**Components:**
- `PluginExtension` and `PluginHost` traits in `crates/pattern_core/src/traits/`
- `CcPluginAdapter` in `crates/pattern_runtime/src/plugin/cc_adapter.rs` — implements `PluginExtension` by wrapping CC plugin directory. Runs in-process via IRPC in-process mode.
- CC artifact translation: skills → Skill blocks (PluginInstalled trust tier), agents → spawn configs, hooks → `on_event()` dispatch via alias table, monitors → Port implementations, commands → utilities, `bin/` → PATH additions
- CC compatibility Haskell library — maps CC terminology to pattern terminology in agent context
- `PluginHost` stub on CC adapter — returns `PluginError::NotSupported` for all methods
- CC subprocess management — spawn pipe processes for commands, manage MCP server connections

**Dependencies:** Phase 1 (manifest, registry). Plan 2 (Skill blocks with PluginInstalled trust tier). v3-sandbox-io (Port trait for monitor wrapping).

**Done when:** A CC-format plugin installed and wrapped by `CcPluginAdapter` loads correctly. Its skills become Skill blocks. Its hooks dispatch through `on_event()`. Its monitors become subscribable ports. CC compatibility library included in agent prelude.
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: Hook lifecycle system

**Goal:** Comprehensive event taxonomy, per-event sync/async semantics, CC alias mapping.

**Components:**
- `HookEvent` enum in `crates/pattern_runtime/src/plugin/hooks.rs` — all lifecycle events
- Hook dispatch system — evaluate registered hooks per event, blocking or notification semantics per event type
- CC event alias table — maps CC event names to pattern events at plugin load time
- Hook binding storage — per-plugin in registry, per-persona/project in config
- Integration points in `agent_loop.rs`, handler dispatch, compaction pipeline — fire events at appropriate points

**Dependencies:** Phase 2 (plugin loading, hook binding registration).

**Done when:** All documented lifecycle events fire at correct points. Blocking events wait for hook completion. Notification events fire-and-forget. CC alias mapping works for pipe-transport plugins.
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: MCP inverted surface

**Goal:** Fold MCP client into runtime, implement three-level awareness model, `ctx.mcp.*` SDK surface.

**Components:**
- MCP client migration — move rmcp-based implementation from `crates/pattern_mcp/` into `crates/pattern_runtime/src/mcp/`
- MCP handler implementation in `crates/pattern_runtime/src/sdk/handlers/mcp.rs` — replacing the stub with `ctx.mcp.call`, `ctx.mcp.list_servers`, `ctx.mcp.introspect`
- System reminder generation — on server load, inject overview pseudo-message into segment 2
- Tool doc materialization — on server load, create Working-tier blocks at `mcp/<server>/<tool>.md` via pattern_memory
- MCP server lifecycle management — spawn, connect, introspect, disconnect, restart, health monitoring
- Delete MCP server stub from `crates/pattern_mcp/`
- Per-server capability integration — MCP access in CapabilitySet, KDL config for per-server/per-tool policy

**Dependencies:** Phase 3 (hooks — MCP server lifecycle fires hook events). Plans 1+2 (memory blocks for doc materialization, skill loading for doc retrieval).

**Done when:** Agents interact with MCP servers through `ctx.mcp.call` only. Tool docs appear as searchable blocks. System reminders show available servers. Old `pattern_mcp` server stub deleted. Per-server permissions enforced.
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: IRPC transport and plugin SDK

**Goal:** IRPC-based out-of-process plugin communication, `McpPluginAdapter`, `pattern-plugin-sdk` crate.

**Components:**
- IRPC serialisation of `PluginExtension` trait in `crates/pattern_runtime/src/plugin/irpc.rs` — `ports()`, `on_event()`, `library()`, port `call`/`subscribe` over IRPC
- `PluginHost` IRPC server — accepts plugin callbacks for memory/message/task access over the same bidirectional channel
- `McpPluginAdapter` in `crates/pattern_runtime/src/plugin/mcp_adapter.rs` — wraps standalone MCP servers as `PluginExtension` implementations (MCP tools → port calls, MCP resources → port subscribe)
- `pattern-plugin-sdk` crate at `crates/pattern_plugin_sdk/` — IRPC server scaffold, typed `PluginHost` client handle, trait re-exports for plugin authors
- ALPN protocol negotiation (`pattern-plugin/1`)
- Cryptographic per-plugin auth via iroh node identity (local); atproto-backed mutual auth (remote)

**Dependencies:** Phase 2 (plugin trait boundary). Phase 3 (hooks — IRPC plugins receive hook events). v3-sandbox-io (Port trait).

**Done when:** A native IRPC plugin can register ports, receive hook events, provide a Haskell library, and call back to pattern for memory/message/task operations. `McpPluginAdapter` wraps MCP servers as `PluginExtension`. `pattern-plugin-sdk` crate provides ergonomic plugin authoring with minimal dependencies.
<!-- END_PHASE_5 -->

<!-- START_PHASE_6 -->
### Phase 6: Trust enforcement and integration

**Goal:** Plugin-installed trust tier enforcement, capability scoping, end-to-end smoke test.

**Components:**
- Trust tier enforcement — `PluginInstalled` trust tier code path activated (Plan 2 reserved the enum value, this plan provides the assignment logic)
- Per-plugin capability scoping — plugin manifest declares required capabilities, user override per-plugin in KDL config
- Ad-hoc skill trust flow — body-redact + user-enable on first use for non-plugin skills
- End-to-end smoke test at `crates/pattern_runtime/tests/plugin_smoke.rs` — install CC-format plugin via pipe, install IRPC plugin, verify skill trust tiers, hook events, MCP inverted surface, capability enforcement
- Cleanup — delete `crates/pattern_mcp/` (fully absorbed into runtime), update workspace Cargo.toml

**Dependencies:** All previous phases.

**Done when:** Plugin-installed skills get correct trust tier. Plugin capabilities scoped per manifest + user config. Smoke test exercises all three transport tiers. `pattern_mcp` crate removed from workspace. Full extensibility surface works end-to-end.
<!-- END_PHASE_6 -->

## Execution mode recommendation

**Collaborative.** The plugin trait boundary and IRPC integration involve novel protocol design where getting the contract right matters more than production speed. The MCP inverted surface is explicitly called out as novel in the v3 design draft's risk section. Human check-in points between phases — particularly after Phase 2 (trait boundary finalized) and Phase 5 (IRPC working) — will catch integration issues before they compound.

## Additional considerations

**MCP crate dissolution.** `pattern_mcp` is absorbed into `pattern_runtime` during Phase 4. The MCP client code (rmcp integration, transport management) moves; the server stub is deleted. Workspace `Cargo.toml` members list updated. Any crates that imported `pattern_mcp` directly are rewired.

**IRPC version pinning.** `irpc` is at v0.13.0 and actively developed. Pin the version in Cargo.toml and wrap behind an internal adapter module so upstream API churn doesn't cascade. Same approach as Plan 1's jj CLI wrapping rationale — wrap the unstable interface, absorb changes in one place.

**Plugin SDK as a separate crate.** `pattern-plugin-sdk` is a new crate in the workspace, intended for plugin authors to depend on. It must have a minimal dependency footprint — only `irpc`, `iroh`, `postcard`, `serde`, and the trait definitions. It should NOT pull in `pattern_runtime` or `pattern_memory`. The trait types it needs from `pattern_core` should be re-exported or duplicated to keep the dependency graph clean for external consumers.

**CC plugin format evolution.** CC's plugin format has expanded to 17+ lifecycle events and includes LSP server support, background monitors, and channels. The design maps CC's current event set and monitor format. LSP server support is a future consideration. Channels (Telegram/Slack/Discord-style message injection) map naturally to Port subscriptions.

**CC compatibility Haskell library.** CC plugin documentation references CC terminology (tools, commands, MCP servers). The `CcPluginAdapter` includes a Haskell compatibility library compiled into the agent's prelude when a CC plugin is loaded. This library maps CC names to pattern names where they differ, so agents working with CC plugins see familiar vocabulary even though the underlying semantics are pattern-native.

**IRPC atproto auth record design.** Published records must be opaque — they should not reveal the counterpart's DID, the plugin's purpose, or the nature of the connection. Records contain only: the iroh node ID (formatted as an at-uri-shaped string so constellation's backlink indexer can index it), a creation timestamp, and a signature over the canonical dag-cbor form. The counterpart DID is determined out-of-band (local config) or via constellation backlink queries ("who else published a record targeting this node ID?"). This gives opt-in discovery without broadcasting relationship information publicly. Detailed record schema and the lexicon definition are implementation-time concerns — the design constraint is: records are opaque, node-key-only, backlink-discoverable. When atproto ships permissioned data, these records migrate from public-but-opaque to permission-gated with no structural changes — just a permission flag on the record. Periodic node key rotation (both sides generate new keys, publish new records, validate, drop old ones) is a natural extension to reduce compromise surface — cost is two record writes + one reconnection, cheap enough for scheduled or event-triggered rotation.
