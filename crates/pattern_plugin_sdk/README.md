# pattern-plugin-sdk

SDK for authoring Pattern plugins.

Plugins implement the [`PluginExtension`] trait. Two execution modes:

- **In-process** — the runtime calls plugin methods directly via trait dispatch.
  Used by built-in adapters (CC, MCP). Zero serialization overhead.
- **Out-of-process** — plugin runs as its own process, communicates with the
  Pattern daemon over IRPC on iroh QUIC. Used by third-party plugins. Adds
  process supervision + crypto auth via iroh node identity.

## Features

- `default = []` — slim surface, no loro / no genai pulled in.
- `memory-sync` — opt-in. Re-exports `MemoryStore` trait + (eventually) `LoroDoc`
  for plugins participating in memory delta sync. Pulls loro through
  `pattern-core`'s `memory` feature.

## Status

Phase 6 of v3-extensibility. Tasks 1-2 landed; tasks 3-8 (wire protocols,
transports, McpPluginAdapter, smoke + integration tests) pending.
