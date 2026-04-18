# Pattern Haskell SDK

Source of truth for the Pattern agent-SDK effect algebras.

## Layout

- `Pattern/Time.hs`, `Pattern/Log.hs`, `Pattern/Display.hs` — fully
  implemented in Phase 3 (Rust handlers at
  `crates/pattern_runtime/src/sdk/handlers/{time,log,display}.rs`).
- `Pattern/Memory.hs` — GADT declared; Rust handler wired end-to-end
  against `Arc<dyn MemoryStore>`. Includes `GetShared` for cross-agent
  shared-block access (Phase 5).
- `Pattern/Search.hs` — scoped search across message history and
  archival entries (SearchMessages/SearchArchival/SearchAll). Phase 5.
- `Pattern/Recall.hs` — archival-entry CRUD with optional scope
  (RecallInsert/RecallSearch/RecallGet/RecallDelete). Phase 5.
- `Pattern/Message.hs` — GADT declared; Send/Reply/Notify wired to
  the router registry (Phase 5 Task 20). Ask is stubbed.
- `Pattern/Shell.hs`, `Pattern/File.hs`, `Pattern/Sources.hs`,
  `Pattern/Mcp.hs`, `Pattern/Rpc.hs`, `Pattern/Spawn.hs` — stubs
  pending their respective post-foundation plans.
- `Pattern/Prelude.hs` — convenience re-export of the full 13-module
  SDK surface.

## Parity with Rust

Each Haskell variant name corresponds byte-for-byte to a Rust variant
name in `crates/pattern_runtime/src/sdk/requests/`. Drift is caught by
the parity test in `sdk::requests` (Task 8). When you rename a Haskell
constructor here, update the Rust `#[core(name = "...")]` attribute and
the parity table simultaneously.

## Runtime resolution

At `Session::open` the runtime resolves a `SdkLocation` (Task 11) to
locate these modules:

1. `PATTERN_SDK_DIR` env var (if set).
2. `concat!(env!("CARGO_MANIFEST_DIR"), "/haskell")` — this directory.

Phase 3 only implements `SdkLocation::Directory`. `Embedded` and `Auto`
are declared for API stability; use `Directory` until the
post-foundation SDK-distribution plan lands.

## Why Haskell?

See `docs/design-plans/2026-04-16-v3-foundation.md`. Tidepool JITs pure
Haskell Core via `tidepool-extract`; freer-simple gives us algebraic
effects that compile to tagged yields the Rust handler side dispatches.
Agent programs stay pure; side effects go through the SDK.
