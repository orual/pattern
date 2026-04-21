# CLAUDE.md - Pattern Memory

Memory subsystem implementation crate. Owns `MemoryCache` (the canonical
`MemoryStore` implementation), `SharedBlockManager`, and schema template
constructors. `StructuredDocument` lives in `pattern_core::memory::document`
(it appears in `MemoryStore` trait signatures; moving it here would create a
circular dependency).

## Dependency rule

`pattern_memory` depends on `pattern_core` and `pattern_db`. Nothing flows
back: `pattern_core` must never depend on `pattern_memory`.

## Testing

- Unit tests: in-file `#[cfg(test)] mod tests` blocks.
- Integration tests: `tests/` directory.
- Run: `cargo nextest run -p pattern-memory`.

## Status

Created 2026-04-19 during v3-memory-rework Phase 1; populated incrementally
in Phases 1-8.
