# Phase A — OOP Plugin Host Substrate

**Plan:** docs/implementation-plans/2026-05-21-plugin-completion/phase_A_oop_host_substrate.md
**Status:** drafted 2026-05-21. Not started.
**Depends on:** nothing (this is the substrate everything else builds on).
**Unblocks:** Phase B (permissions), Phase C (CC adapter completion needs HostApi for any callbacks it offers).

## Motivation

Plugins-as-OOP-processes is the canonical extensibility path (discord plugin lives there today). Currently the **host-side dispatcher** (`pattern_runtime/src/plugin/host_handler.rs`, 57 lines) is a v1 stub: every variant returns `Unimplemented` or empty `Vec`. The corresponding **plugin-side caller** (`pattern_runtime/src/plugin/transport/out_of_process.rs`) is also partial — only `declare_ports` + `get_library` are wired end-to-end.

Without this substrate, plugins can't call back into Pattern for memory/task/skill operations from their port handlers. Discord plugin happens to not need this (it only sends messages outbound, doesn't read memory), but anything more sophisticated is blocked.

## Current state (as of 2026-05-21)

### `pattern_runtime/src/plugin/host_handler.rs` — 17 stubbed variants

12 return `Err(WirePluginError::Unimplemented { method })` or `Err(WireMemoryError::Unimplemented { method })`:
- `HostSendMessage`, `HostTaskCreate`, `HostTaskTransition`, `HostTaskLink`, `HostSkillInvoke`
- `MemoryCreateBlock`, `MemoryDeleteBlock`, `MemoryPersist`, `MemoryUpdateMetadata`, `MemoryUndoRedo`, `MemoryGetSharedBlock`, `MemoryInsertArchival`, `MemoryDeleteArchival`

4 return empty `Vec` (no way to distinguish empty from not-implemented — bug):
- `HostTaskQuery` → `Vec<WireTaskItem>`
- `MemorySearch` → `Vec<WireSearchResult>`
- `MemoryListBlocks` → `Vec<BlockMetadata>`
- `MemorySearchArchival` → (need to check exact type)

### `pattern_runtime/src/plugin/transport/out_of_process.rs` (423 lines)

Doc comment confirms: "V1 wires declare_ports + library end-to-end (smallest payloads, no PluginContext conversion needed) and leaves the rest as `Unimplemented` returns. Full method dispatch lands when the integration test fixture plugin exists (tasks 7-8)."

Methods on `PluginGuestProtocol` that need real dispatch from this side:
- `OnInstall`, `OnEnable`, `OnDisable` (lifecycle)
- `OnHookEvent`, `OnHookEventBlocking` (hook dispatch)
- `PortCall`, `PortSubscribe`, `PortUnsubscribe` (port ops)

## Target state

All 17 host-side variants dispatch into the runtime registry + return real values. All daemon-side `OutOfProcessPluginConnection` methods send their corresponding wire requests + map responses back to pattern-core types. End-to-end test exercises both directions through a fixture plugin.

## Tasks (bite-sized, frequent commits)

### A.1 — Result-wrap the 4 Vec variants
- Change protocol.rs:
  - `HostTaskQuery: tx = oneshot::Sender<Vec<WireTaskItem>>` → `Result<Vec<WireTaskItem>, WirePluginError>`
  - `MemorySearch: tx = oneshot::Sender<Vec<WireSearchResult>>` → `Result<Vec<WireSearchResult>, WireMemoryError>`
  - `MemoryListBlocks: tx = oneshot::Sender<Vec<BlockMetadata>>` → `Result<Vec<BlockMetadata>, WireMemoryError>`
  - `MemorySearchArchival: tx = oneshot::Sender<Vec<...>>` → `Result<Vec<...>, WireMemoryError>`
- Update host_handler stubs to return `Err(Unimplemented)` consistently (no more silent empty-Vec)
- Update transport/out_of_process.rs callers to handle the Result
- Commit: `chore(plugin): Result-wrap Vec-returning host protocol variants`

### A.2 — Wire real dispatch into host_handler
Each variant gets a real implementation. Handler needs access to runtime state (memory store, task graph, agent registry, skill loader). Currently `spawn()` takes no args; needs to accept a `HostApiContext { memory, tasks, skills, registry, ... }` or equivalent.

Sub-tasks (one per variant group, each a commit):
- **A.2a HostSendMessage**: dispatch into `agent_registry.send_to(agent_id, ...)`
- **A.2b HostTask\*** (Create/Transition/Link/Query): dispatch into Tasks effect handler equivalents
- **A.2c HostSkillInvoke**: load + invoke via skill registry
- **A.2d Memory\*** (Create/Delete/Search/ListBlocks/Persist/UpdateMetadata/UndoRedo/GetSharedBlock/InsertArchival/SearchArchival/DeleteArchival): dispatch into memory store

### A.3 — Wire daemon-side `OutOfProcessPluginConnection` methods
Counterpart of A.2 from the daemon-dialing-the-plugin side. Each `PluginGuestProtocol` method beyond `DeclarePorts`/`GetLibrary` needs real wire dispatch + PluginContext conversion.

Sub-tasks:
- **A.3a Lifecycle**: OnInstall/OnEnable/OnDisable — convert PluginContext → WirePluginContext + dial
- **A.3b Hooks**: OnHookEvent (fire-forget) + OnHookEventBlocking (await WireHookResponse)
- **A.3c Ports**: PortCall (oneshot), PortSubscribe (stream with Done), PortUnsubscribe

### A.4 — Integration suite at `crates/pattern_runtime/tests/plugin_transport.rs`
Build the existing-but-deferred Task 8. Fixture plugin that:
- Implements all PluginGuestProtocol methods (echo-style or trivial)
- Makes one of each PluginHostProtocol callback during on_enable
- Test asserts both directions succeed + Drop cleanup works + route-table population matches

Tests:
- In-process variant (existing test infrastructure)
- OOP variant (spawn the fixture binary, dial it, exercise calls)
- Concurrent variant (two plugins active simultaneously, no cross-talk)

## Acceptance criteria

- All 17 host_handler variants return real values, not `Unimplemented` (verified by grep + by test)
- All 4 Vec-returning variants are Result-wrapped at the protocol layer
- All `OutOfProcessPluginConnection` methods dispatch real wire requests, not `Unimplemented`
- Integration suite in plugin_transport.rs passes for in-process + OOP + concurrent scenarios
- Discord plugin still works end-to-end after the changes (regression check)
- `cargo nextest run -p pattern-runtime` passes

## Gotchas / things to be careful about

- **PluginContext ↔ WirePluginContext conversion** is partial today. Each side has a half-implemented mapping; gaps will surface when wiring A.3a. Don't band-aid — fix the conversion both ways when needed.
- **Memory.append database-locked misleading error** (2026-05-20 operational note): if writes race during test setup, retry can produce duplicates. Use serial test attribute or sequence writes.
- **structured tracing::error before convert** (constitution-coding): every `Unimplemented` → real call should emit a `tracing::debug` at dispatch start + `tracing::error` on failure with the wire error variant. Don't swallow errors silently.
- **Stream Done variants** for PortSubscribe: producer must send `WirePortStreamItem::Done` before dropping (already established, but easy to forget when wiring the new dispatch).
- **postcard + skip_serializing_if** structurally incompatible — if Result-wrapping introduces new Option fields anywhere on the wire types, drop the skip_serializing_if attrs.
- **iroh 1.0.0-rc.0 API drift**: don't trust prior memory of method names; verify against current source if anything in EndpointAddr / TransportAddr changes shape.

## Out of scope (deferred to later phases)

- Permissions plumbing (phase B)
- CC adapter completion (phase C)
- TUI auth (phase D)
- Remote plugins via atproto (phase E)
- MemorySyncProtocol full implementation (separate ALPN, separate phase later)

## Verification before declaring done

1. `cargo nextest run -p pattern-runtime` — all green
2. `cargo nextest run --workspace` — no regressions
3. Discord plugin live test: send DM, get response. (Discord doesn't exercise host callbacks but it does exercise port subscription which is in A.3c.)
4. Fixture plugin end-to-end: spawn, on_enable, make each host callback, assert all succeed.
5. `grep -E 'Unimplemented|Vec::new\(\)' host_handler.rs` returns zero matches in match arms.
