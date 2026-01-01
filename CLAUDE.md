# Project Overview

Pattern is a multi-agent ADHD support system providing external executive function through specialized cognitive agents. Each user ("partner") gets their own constellation of agents.

**Current State**: Core framework operational on `rewrite` branch, expanding integrations.


> **For AI Agents**: This is the source of truth for the Pattern codebase. Each crate has its own `CLAUDE.md` with specific implementation guidelines.

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

```
pattern/
├── crates/
│   ├── pattern_api/      # Shared API types and contracts
│   ├── pattern_auth/     # Credential storage (ATProto, Discord, providers)
│   ├── pattern_cli/      # CLI with TUI builders
│   ├── pattern_core/     # Agent framework, memory, tools, coordination
│   ├── pattern_db/       # SQLite with FTS5 and vector search
│   ├── pattern_discord/  # Discord bot integration
│   ├── pattern_mcp/      # MCP client and server
│   ├── pattern_nd/       # ADHD-specific tools and personalities
│   └── pattern_server/   # Backend API server
├── docs/                 # Architecture docs and guides
└── justfile              # Build automation
```

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

- Use `mod.rs` to re-export public items only.
- No nontrivial logic in `mod.rs`—use `imp.rs` or specific submodules.
- Keep module boundaries strict with restricted visibility.
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

# Database operations (from crate directory!)
cd crates/pattern_db && cargo sqlx prepare
cd crates/pattern_auth && cargo sqlx prepare
# NEVER use --workspace flag with sqlx prepare
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
- **sqlx**: Compile-time verified SQL queries.
- **loro**: CRDT for versioned memory blocks.
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
