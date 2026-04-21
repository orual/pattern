# Pattern v3 Extensibility Design

## Summary
<!-- TO BE GENERATED after body is written -->

## Definition of Done

Plan 4 in the Pattern v3 rewrite sequence. Builds the plugin system, MCP inverted surface, iroh-rpc transport, and hook lifecycle on top of Plans 1-3. Consumes Plan 3's capability-based permission system for trust enforcement. The plan is done when:

### CC-compatible plugin loader

- Plugin manifest format (`plugin.json`) parsed with pattern-specific extensions under `pattern` namespace.
- Loader translates CC plugin artifacts to pattern primitives:
  - CC agents → pattern agents (default `spawn_mode: ephemeral`; opt-in to persona via `pattern.persona_mode`)
  - CC skills → pattern Skill blocks (trust-tagged `PluginInstalled` — the tier reserved in Plan 2)
  - CC commands → pattern utilities with audience tier per declaration
  - CC hooks → pattern lifecycle hooks via alias table
- Unknown fields silently ignored (bidirectional non-breaking with stock CC hosts).
- Plugin directory layout: `~/.pattern/plugins/<plugin-id>/` (global), `<project>/.pattern/shared/plugins/<plugin-id>/` (project-scoped committed), `<project>/.pattern/private/plugins/<plugin-id>/` (project-scoped private, gitignored).
- Load precedence: project > global > ambient. Collisions warned at install.

### MCP inverted surface (client-only)

- Agents see a single primitive: `ctx.mcp.call(server, method, args)` plus discovery (`ctx.mcp.list_servers()`, `ctx.mcp.introspect(server)`).
- Tool documentation materialized as searchable memory blocks at `mcp/<server>/tools/<tool>.md` on server load. Hybrid FTS+vector search, loaded on demand, never implicitly in context.
- Existing MCP client (rmcp-based, stdio/HTTP/SSE) wired through the inverted surface.
- Per-server scoped permissions integrated with Plan 3's capability system.
- MCP server stub removed from the codebase.
- MCP crate placement determined during brainstorming (stays separate, folds into runtime, or core trait + runtime impl).

### iroh-rpc transport

- iroh-rpc over QUIC as a plugin transport tier alongside pipe and MCP.
- Works for local communication (QUIC-over-loopback).
- Cryptographic per-plugin auth via iroh node identity.
- Plugin manifest declares transport preference via `pattern.transport` field.
- One plugin can expose multiple transports (e.g., MCP for tools + iroh-rpc for a DataStream).

### Hook lifecycle system

- Pattern-native lifecycle events fired at appropriate points:
  - `persona.attach.<project>`, `persona.detach`
  - `turn.before`, `turn.after`
  - `tool.before`, `tool.after`
  - `memory.write`
  - `fork.spawn`, `fork.resolve`
  - `compaction.cycle.start`, `compaction.cycle.end`
  - `plugin.install`, `plugin.uninstall`
- CC event aliases mapped: `SessionStart` → `persona.attach`, `UserPromptSubmit` → `turn.before`, `PreToolUse`/`PostToolUse` → `tool.before`/`tool.after`, etc.
- Hooks fire in standard pathway; cannot bypass runtime capability gates.

### Trust enforcement

- Plugin-installed skills assigned `PluginInstalled` trust tier (the value reserved in Plan 2, now with an actual code path).
- Plugin effects respect Plan 3's capability system — plugin capabilities are scoped per plugin manifest declarations + user override per-plugin.
- Ad-hoc skills (non-plugin source) follow body-redact + user-enable flow on first use.
- Plugin MCP servers get same MCP permissions model as CC.

### Plugin capabilities

- Plugins can register: agents, skills, commands, hooks, MCP servers, DataStream implementations, MessageRouter endpoints.
- All registration via `pattern_plugin` loader, bound at load time.
- DataStream + MessageRouter endpoints registered dynamically by plugins.

### Plugin transports (tiered)

- **Tier 1**: stdin/stdout pipe — CC compat commands, one-shot tools. Functional.
- **Tier 2**: MCP (stdio/SSE/streamable-HTTP) via rmcp — CC-standard plugins, resource subscriptions for data streams. Functional.
- **Tier 3**: iroh-rpc over QUIC — richer bidirectional integration, persistent event streams. Functional.
- **Tier 4 (WASM)**: explicitly out of scope (v2+).

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
- `docs/plans/2026-04-16-rewrite-v3-design-draft.md` §5 (plugin layer brainstorm)

## Acceptance Criteria
<!-- TO BE GENERATED and validated before glossary -->

## Glossary
<!-- TO BE GENERATED after body is written -->
