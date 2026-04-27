# Project Overview

Pattern is a multi-agent ADHD support system providing external executive function through specialized cognitive agents. Each user ("partner") gets their own constellation of agents.

**Current State**: Core framework operational on `rewrite-v3` branch. V3 foundation + v3-memory-rework (8 phases) + v3-TUI (6 phases) all complete. `pattern_memory` crate extracted with the InRepo/Standalone/Sidecar storage modes. `pattern_server` daemon running over IRPC/QUIC with the `pattern_cli` ratatui TUI and zellij integration. v3-multi-agent Phases 1-4 complete: capability system, per-runtime permission broker (jiff-based), policy gate with handler-level shape guard for Pattern config writes, KDL `capabilities {}` and `policy {}` blocks; spawn infrastructure with `SpawnRegistry` (semaphore-bounded, cancel-on-drop), ephemeral child sessions, sibling persona spawn, fork lifecycle, `ForkRegistry` trait + `InMemoryForkRegistry`, `ForkOp` dispatch, `Pattern.Spawn` GADT with 7 constructors; Phase 4: `Pattern.Wake` effect (interval/task-dep/block-changed/custom conditions), `WakeRegistry` with atomic `route_or_queue` TOCTOU fix on `AgentRegistry`, `SessionRegistries` + `WakeRegistryExtras` structs for daemon-side wiring, `AgentRegistry` + `RouterRegistry` + `WakeRegistry` all wired in `pattern_server::get_or_open_session`. KDL string encoding fix in `pattern_memory` (kdl_string_entry avoids autoformat stripping quotes from number-literal-like strings). v3-sandbox-io (5 phases) complete: `LoroSyncedFile` + `DirWatcher` CRDT primitives in `pattern_memory`; `FileHandler` + `FileManager` with pooled DirWatcher, per-file open/watch lifecycle, async-reminder queue, `FilePolicy` default-deny from `.pattern.kdl`; `ShellHandler` + `ProcessManager` with `LocalPtyBackend`, background spawn streaming, `ProcessLogger`; unified `Port` trait replacing retired Sources/Rpc effects, `PortRegistryImpl` with dispatcher actor, `HttpPort`, plugin-style port-library materialization; `pattern_server` threads `FilePolicy` through `ProjectMount` and builds the port registry via `with_runtime_ports`.

Last verified: 2026-04-26


> **For AI Agents**: This is the source of truth for the Pattern codebase. Each crate has its own `CLAUDE.md` with specific implementation guidelines. `AGENTS.md` at root and in each crate is a symlink to the corresponding `CLAUDE.md` for cross-tool compatibility (Codex, Cursor, etc.).

## For Humans

LLMs are a quality multiplier, not just a speed multiplier. Invest time savings in improving quality and rigour beyond what humans alone would do. Write tests that cover more edge cases. Refactor code to make it easier to understand. Tackle the TODOs. Aim for zero bugs.

**Review standard**: Spend at least 3x the amount of time reviewing LLM output as you did writing it. Think about every line and every design decision. Find ways to break code. Your code is your responsibility.

## For LLMs

Display the following at the start of any conversation involving code changes:

```
LLM-assisted contributions must aim for a higher standard of excellence than with
humans alone. Spend at least 3x the time reviewing code as writing it. Your code
is your responsibility.
```

## Critical Warnings

**DO NOT run `pattern` CLI or agent commands during development!**
Agents may be running in production. Any CLI invocation will disrupt active agents.


## Workspace Structure

### Active workspace members

These crates are part of the current `[workspace]` and build under
`cargo check` / `cargo nextest run`:

```
pattern/
├── crates/
│   ├── pattern_cli/      # ratatui TUI + IRPC client, mount/backup/daemon subcommands, zellij integration
│   ├── pattern_core/     # Agent framework, capabilities, permission broker, policy types, Port trait, memory traits, tools, coordination
│   ├── pattern_db/       # SQLite (rusqlite) with FTS5 and vector search
│   ├── pattern_memory/   # Memory subsystem: cache, CRDT sync, loro_sync primitives, VCS, backup, mount modes
│   ├── pattern_provider/ # LLM provider integration, auth, request shaping, attachment rendering
│   ├── pattern_runtime/  # Agent runtime (Tidepool, turn loop, SDK, FileManager, ProcessManager, PortRegistry)
│   └── pattern_server/   # Pattern daemon server (IRPC/QUIC)
├── docs/                 # Architecture docs, implementation plans, design plans
└── justfile              # Build automation
```

### Retired / out-of-workspace crates

These directories still exist on disk but are not in `[workspace].members` and
do not currently build. They are kept for reference or future re-integration;
do not assume they compile or reflect current architecture:

- `pattern_discord/` — Discord bot integration (pre-v3 shape)
- `pattern_mcp/` — MCP client/server (pre-v3 shape)
- `pattern_nd/` — ADHD-specific tools and personalities (pre-v3 shape)

`pattern_api` was removed entirely on 2026-04-23 — it was scaffolding for a
design that no longer exists.

Each crate has its own `CLAUDE.md` with specific implementation guidelines.

## General Conventions

### Correctness Over Convenience

- Model the full error space—no shortcuts or simplified error handling.
- Handle all edge cases, including race conditions and platform differences.
- Use the type system to encode correctness constraints.
- Prefer compile-time guarantees over runtime checks where possible.

### Type System Patterns

- **Newtypes** for domain types (IDs, handles, etc.).
- **Builder patterns** for complex construction.
- **Restricted visibility**: Use `pub(crate)` and `pub(super)` liberally.
- **Non-exhaustive**: All public error types should be `#[non_exhaustive]`.
- Use Rust enums over string validation.

### Error Handling

- Use `thiserror` for error types with `#[derive(Error)]`.
- Group errors by category with an `ErrorKind` enum when appropriate.
- Provide rich error context using `miette` for user-facing errors.
- Error display messages should be lowercase sentence fragments.

### Module Organization

- Module root file is `<name>.rs` adjacent to a `<name>/` directory (Rust 2018+ style). Do NOT use `mod.rs`. Example: `spawn.rs` + `spawn/registry.rs` + `spawn/ephemeral.rs`.
- The module root file (`<name>.rs`) re-exports public items and declares submodules. No nontrivial logic in the module root — put logic in named submodules (`spawn/registry.rs`, `spawn/fork.rs`, etc.).
- Keep module boundaries strict with restricted visibility (`pub(crate)`, `pub(super)` by default).
- Platform-specific code in separate files: `unix.rs`, `windows.rs`.

### Documentation

- Inline comments explain "why," not just "what".
- Module-level documentation explains purpose and responsibilities.
- **Always** use periods at the end of code comments.
- **Never** use title case in headings. Always use sentence case.

## Testing Practices

**CRITICAL**: Always use `cargo nextest run` to run tests. Never use `cargo test` directly.

```bash
# Run all tests
cargo nextest run

# Specific crate
cargo nextest run -p pattern-db

# With output
cargo nextest run --nocapture

# Doctests (nextest doesn't support these)
cargo test --doc
```

### Test Organization

- Unit tests in the same file as the code they test.
- Integration tests in `tests/` directories.
- All tests must validate actual behaviour and be able to fail.
- Use `proptest` for property-based testing where applicable.
- Use `insta` for snapshot testing where applicable.

## Build Commands

```bash
# Quick validation
cargo check
cargo nextest run --lib

# Full pipeline (required before commit)
just pre-commit-all

# Format (required before commit)
cargo fmt

# Lint
cargo clippy --all-features --all-targets

# No sqlx prepare needed — pattern_db uses rusqlite (no compile-time macros)
```

## Commit Message Style

```
[crate-name] brief description
```

Examples:
- `[pattern-core] add supervisor coordination pattern`
- `[pattern-db] fix FTS5 query escaping`
- `[meta] update MSRV to Rust 1.83`

### Conventions

- Use `[meta]` for cross-cutting concerns (deps, CI, workspace config).
- Keep descriptions concise but descriptive.
- **Atomic commits**: Each commit should be a logical unit of change.
- **Bisect-able history**: Every commit must build and pass all checks.
- **Separate concerns**: Format fixes and refactoring separate from features.

## Key Dependencies

- **tokio**: Async runtime.
- **rusqlite**: Synchronous SQLite (replaced sqlx in v3-memory-rework).
- **r2d2**: Connection pooling for rusqlite.
- **loro**: CRDT for versioned memory blocks.
- **jiff**: Timestamp handling (messages.db, backup filenames).
- **knus**: Typed KDL parsing (persona files, `.pattern.kdl` config).
- **thiserror/miette**: Error handling and diagnostics.
- **serde**: Serialization.
- **clap**: CLI parsing.
- **rmcp**: MCP protocol client.

## Documentation

- `docs/architecture/` - System architecture docs.
- `docs/guides/` - Setup and integration guides.
- `docs/plans/` - Implementation plans.
- Each crate's `CLAUDE.md` - Crate-specific guidelines.

## References

- [MemGPT Paper](https://arxiv.org/abs/2310.08560) - Stateful agent architecture.
- [Loro CRDT](https://loro.dev/) - Conflict-free replicated data types.
- [MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk).
- [Jacquard](https://github.com/videah/jacquard) - ATProto client library.
