# Pattern v2: Architecture Rework

## Background

Pattern v1 was written in "a fit of madness" - rapid prototyping that taught us what the actual requirements are. This document captures the vision for v2, which takes those lessons and rebuilds the core data layer properly.

## Problems with v1

### Database (SurrealDB)

1. **Global memory live query** - `subscribe_to_agent_memory_updates` ignores agent_id entirely, watching ALL memory blocks. Any memory update from any agent triggers updates to all agents.

2. **No row-level security** - All isolation is application-level query filtering. One bad query = data bleed between agents.

3. **Memory ownership confusion** - Memories belong to User, not Agent. Multiple agents can share memory blocks if labels collide.

4. **Edge direction inconsistencies** - `group_members` is backwards from all other edge patterns.

5. **Accumulated gotchas** - Datetime serialization issues, ID bracket parsing, LIVE SELECT parameter limitations, etc.

6. **Custom entity macro nightmare** - `#[derive(Entity)]` was built to work around SurrealDB pain points but became its own source of complexity.

### Memory System (DashMap-based)

1. **In-memory caching creates sync problems** - DashMap holds memory state, DB holds another copy, they can desync.

2. **Namespace collisions** - Same label across agents in constellation = last write wins.

3. **No history/versioning** - Memory updates are destructive, no way to see what changed or roll back.

4. **Agents must manually maintain logs** - No system-level rolling logs.

5. **No templated/structured memories** - Everything is opaque strings.

## v2 Goals

### Database Layer

- **SQLite + sqlx** - Boring, reliable, well-understood
- **One database per constellation** - Physical isolation, cross-contamination impossible at storage level
- **Vector search via sqlite-vec** - Extension-based, proven approach
- **No custom entity macro** - Just sqlx queries, maybe light derive helpers
- **Simple relational model** - Foreign keys and junction tables, no graph magic

### Memory Layer

- **Loro CRDT-backed documents** - Every edit tracked, mergeable, time-travel capable
- **No in-memory caching** - DB is single source of truth, read when needed, write immediately
- **Versioned memories** - Agents can see edit history, roll back bad updates
- **Templated memories** - Structured schemas for common patterns
- **Rolling logs** - System-maintained, agents observe but don't manage
- **Diffing/rollback exposed to users** - Maybe to agents too

### Agent Runtime

- **Pattern owns LLM calls** - Not an MCP tool bolted onto something else
- **Context building stays internal** - Deep integration into the context maintenance process
- **Coding harness** - Pattern can be a coding agent runtime
- **ACP support** - Agent Client Protocol for editor integration (Zed, JetBrains, Neovim, etc.)
  - Pattern implements the `Agent` trait from `agent-client-protocol` crate
  - Runs as subprocess, communicates via JSON-RPC over stdio
  - Editors can spawn Pattern agents and interact through standard protocol
  - See v2-api-surface.md for details

### Interface Architecture

v1 had the CLI as the primary interface - it directly instantiated agents, hit the DB, made LLM calls. The server was a stub for future multi-user hosting.

v2 rethinks this:

- **CLI remains important** - Trusted local interaction point. Agent knows CLI input is from their partner human, unlike Discord/Bluesky input from strangers.
- **Server as coordination layer** - HTTP API for external integrations, multi-user hosting, but not the only way to run agents
- **Remote presence connectors** - For coding harness and ACP, agents on a server need to reach into the partner's local environment:
  - Read/write files on partner's machine
  - Connect to local editor instances (LSP integration)
  - CLI could work over such a connector too
  - Enables "agent runs on server, acts on local dev environment" model

The trust model matters: CLI = partner (trusted), Discord/Bluesky = conversant (verify), Remote connector = partner's environment (trusted proxy).

## Non-Goals

- Backwards compatibility at the API level (CAR export/import is the migration path)
- Supporting SurrealDB alongside SQLite
- Preserving the entity macro system

## Migration Path

Existing constellations migrate via:
1. CAR file export from v1
2. CAR file import to v2

The compatibility layer lives at the export/import boundary, not in the runtime.

## Key Design Decisions

### One DB Per Constellation

Benefits:
- Physical isolation - no query can accidentally leak across constellations
- SQLite's single-writer limitation becomes a non-issue (one constellation = one writer)
- Easy backup/restore per constellation
- Natural sharding if we ever need horizontal scale

Trade-offs:
- Cross-constellation queries require opening multiple DBs
- Slightly more complex connection management

### Loro for Memory Documents

Benefits:
- Built-in versioning and history
- CRDT means eventual consistency if we ever need it
- Rich document primitives (text, lists, maps, trees)
- Diff visibility built-in

Trade-offs:
- Another dependency
- Learning curve for the Loro model
- Need to understand how it persists/snapshots

### No In-Memory Cache

Benefits:
- Single source of truth
- No sync bugs possible
- Simpler mental model

Trade-offs:
- More DB reads
- Need to think about query efficiency

## Open Questions

1. ~~How does Loro persistence work?~~ **Answered**: Snapshots + delta updates, both as byte blobs. We store snapshots in SQLite, updates in a separate table, consolidate periodically.
2. ~~What memory primitives does Letta use?~~ **Answered**: Labeled blocks with descriptions, read-only option, shared blocks between agents. See v2-memory-system.md.
3. How do we handle the constellation-level shared state (activity tracker, etc.)?
4. What's the right API surface for the HTTP server?
5. Should agents have write access to their own version history, or is that user-only?
6. Remote presence connector protocol - what does the interface look like?
7. Trust levels for different input sources - how does this affect tool access?

## Related Documents

- [v2-database-design.md](./v2-database-design.md) - SQLite schema, sqlx patterns
- [v2-memory-system.md](./v2-memory-system.md) - Loro integration, templates, shared context
- [v2-migration-path.md](./v2-migration-path.md) - CAR export/import, interactive migration
- [v2-api-surface.md](./v2-api-surface.md) - HTTP endpoints, ACP integration, deployment modes
- [v2-remote-presence.md](./v2-remote-presence.md) - iroh-based connector protocol
- [v2-pattern-dialect.md](./v2-pattern-dialect.md) - Action language for agents
- [v2-dialect-implementation.md](./v2-dialect-implementation.md) - Dialect parser implementation notes

### Future (v2.1+)

- [v2-constellation-forking.md](./v2-constellation-forking.md) - Explicit fork/merge for isolated work

## Existing Infrastructure to Preserve/Adapt

- `realtime.rs` - Event sink/tap system for streaming agent responses to multiple consumers. Good foundation for remote presence streaming.
