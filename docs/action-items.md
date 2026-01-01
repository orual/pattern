# Action items

Generated: 2025-12-31

This document catalogues all TODOs, FIXMEs, stubs, and incomplete implementations found in the Pattern codebase. Items are organized by severity and then by crate.


### Design decisions to revisit

| Location | Issue |
|----------|-------|
| `pattern_core/src/context/builder.rs:944` | Panics on non-Text content - may need graceful handling |
| `pattern_macros/src/lib.rs:1169,1186` | Panics on unknown table names - no fallback |
| `pattern_core/src/memory/store.rs` | MemoryStore trait has many convenience methods (`append_to_block`, `replace_in_block`, etc.) that may be redundant now that StructuredDocument has a richer interface (`import_from_json`, `export_for_editing`, etc.). Consider consolidating - callers can use `get_block()` + document methods + `mark_dirty()`/`persist_block()` directly. |

---

## Stub modules (intentionally incomplete)

These are known stubs that are explicitly documented:

| Module | Status | Notes |
|--------|--------|-------|
| `pattern_mcp/src/server.rs` | STUB | Server functionality not implemented |
| `pattern_mcp/src/client/tool_wrapper.rs` | STUB | MCP tool wrapper placeholder |
| `pattern_mcp/src/client/service.rs` | STUB | Simplified stub with mock tool creation |
| `pattern_cli/src/agent_ops.rs` | PARTIAL STUB | Data source registration pending rework |
| `pattern_cli/src/background_tasks.rs` | STUB | Disabled during migration |
| `pattern_core/CLAUDE.md:121` | STUB | Ollama provider |

---

## Critical (blocking core functionality)


### pattern_core

| Location | Issue |
|----------|-------|
| `src/runtime/context.rs:1493` | Graceful shutdown not implemented - uses `abort()`, in-flight messages may be lost. Needs cancellation tokens. |
| ~~`src/memory/cache.rs:1284`~~ | ~~Memory edit operation incomplete~~ - DONE: CRDT-aware replace implemented |

### pattern_mcp - lower priority than anything in core or cli crate

| Location | Issue |
|----------|-------|
| `src/server.rs:110` | `todo!("Implement MCP server start")` |
| `src/server.rs:116` | `todo!("Implement MCP server stop")` |

### pattern_server - lower priority than anything in core or cli crate

| Location | Issue |
|----------|-------|
| `src/lib.rs:32` | CORS is permissive - needs proper configuration for production |
| `src/handlers/auth.rs:79` | API key authentication not implemented |
| `src/handlers/auth.rs:105` | Token family revocation check not implemented |
| `src/handlers/auth.rs:136-138` | User loading from database - returns hardcoded "TODO" username |

---

## High priority (affects major features)

### pattern_cli

| Location | Issue |
|----------|-------|
| ~~`src/slash_commands.rs:137`~~ | ~~`/list` - reimplement for pattern_db~~ - DONE |
| ~~`src/slash_commands.rs:292`~~ | ~~`/archival` - reimplement for pattern_db~~ - DONE |
| ~~`src/slash_commands.rs:300`~~ | ~~`/context` - reimplement for pattern_db~~ - DONE: uses `prepare_request()` |
| ~~`src/slash_commands.rs:308`~~ | ~~`/search` - reimplement for pattern_db~~ - DONE: uses unified `memory.search()` |
| ~~`src/commands/debug.rs:474`~~ | ~~Show context - reimplement for pattern_db~~ - DONE |
| ~~`src/commands/debug.rs:509`~~ | ~~Edit memory - reimplement for pattern_db~~ - DONE: supports text + structured via TOML |
| ~~`src/commands/debug.rs:548`~~ | ~~Modify memory - reimplement for pattern_db~~ - DONE: label rename, permission, type |
| `src/commands/debug.rs:596` | Context cleanup - SKIPPED: was debug-specific, no longer needed |
| `src/commands/agent.rs:141` | Agent export - reimplement for pattern_db |
| `src/commands/builder/group.rs:170` | Load shared_memory from DB |
| `src/commands/builder/group.rs:171` | Load data_sources from DB or pattern_config JSON |
| `src/commands/builder/agent.rs:586` | Temp file editing for agent builder |
| `src/commands/builder/display.rs:135` | Unicode character splitting can panic |
| `src/endpoints.rs:117` | Refactor print logic to be reusable |
| `src/main.rs:701` | Uncomment when pattern_db is integrated |

### pattern_core

| Location | Issue |
|----------|-------|
| `src/runtime/mod.rs:418` | Cross-agent search permission check - just logs, doesn't enforce |
| `src/runtime/mod.rs:432` | Failed agent search errors silently ignored |
| `src/runtime/mod.rs:445` | Constellation-wide search permission check missing |
| `src/messages/mod.rs:407` | Text + ToolCalls combination not properly implemented |
| `src/messages/mod.rs:472` | Multiple content items combination not properly implemented |
| `src/coordination/patterns/pipeline.rs:132` | Parallel processing not implemented |
| `src/coordination/patterns/round_robin.rs:225` | Response collection returns empty vec |
| `src/coordination/patterns/dynamic.rs:316,474` | Response content collection returns empty vec |
| `src/coordination/patterns/sleeptime.rs:296,539` | Response content collection and constellation activity check incomplete |
| `src/tool/builtin/constellation_search.rs:431,448` | Role/time filtering parameters accepted but not used |
| `src/agent/db_agent.rs:2201,2438` | Tests disabled - need `runtime.prepare_request()` continuation support |
| `src/data_source/bluesky/inner.rs:544` | Should check by block_id when handle changes |
| `src/data_source/bluesky/inner.rs:581` | Label update when handle changes needs DB method |
| `src/config.rs:113` | Custom block source inventory lookup not implemented |
| `src/config.rs:151` | DiscordSource::from_config not implemented |
| `src/config.rs:156` | Custom stream source inventory lookup not implemented |
| `src/export/tests.rs:1109` | Add `#[serde(with = "serde_bytes")]` to SnapshotChunk and MemoryBlockExport |

### pattern_discord

| Location | Issue |
|----------|-------|
| `src/bot.rs:1653` | Full database mode with user lookup not implemented |
| `src/bot.rs:1655` | Full mode not yet implemented |
| `src/endpoints/discord.rs:846` | Reply context present but not implemented |

### pattern_server - lower priority than anything in core or cli crate

| Location | Issue |
|----------|-------|
| `src/handlers/mod.rs:24` | Remaining endpoints not added |
| `src/handlers/health.rs:10` | Uptime tracking returns hardcoded 0 |
| `src/auth/atproto.rs:394` | Profile fetch from PDS using session not implemented |
| `CLAUDE.md:180` | Rate limiting on auth endpoints |

### pattern_mcp - lower priority than anything in core or cli crate

| Location | Issue |
|----------|-------|
| `src/client/transport.rs:97,146` | Custom headers beyond Authorization need custom client implementation |
| `src/client/transport.rs:124,175` | OAuth authentication not implemented |


---

## Medium priority (features and improvements)

### pattern_core

| Location | Issue |
|----------|-------|
| `src/agent/mod.rs:205` | Pass along and use field (unclear what) |
| `src/config.rs:1300` | Make default base instructions easier to extend |
| `src/runtime/endpoints/mod.rs:46` | Implement AgentSessionExt traits for BlueskyAgent |
| `src/tool/builtin/tests.rs:142` | Rewrite archival entry test - immutable entries |

### pattern_macros  - legacy crate for surrealdb, only used by pattern_surreal_compat

| Location | Issue |
|----------|-------|
| `src/lib.rs:863` | Load `Option<EdgeEntity>` relation |
| `src/lib.rs:936` | Load single EdgeEntity relation |

---

## Low priority

### pattern_core

| Location | Issue |
|----------|-------|
| `src/tool/builtin/test_utils.rs:133` | Log or aggregate errors from failed agent searches |


---

## Not implemented (documented but planned)

### pattern_discord

Per `crates/pattern_discord/CLAUDE.md`:
- Many slash commands in progress
- Natural language command routing (planned)

### pattern_server

Per `crates/pattern_server/CLAUDE.md`:
- WebSocket support
- Most API endpoints beyond auth
- Agent management endpoints
- Message handling endpoints
- Group coordination endpoints
- MCP integration
- Metrics and monitoring

---

### Documentation TODOs

Plan documents in `docs/plans/` contain many TODOs, but these are implementation notes for future work rather than incomplete code. They are not included in this list.
