# v3-extensibility Phase 6: IRPC plugin transport + plugin SDK + McpPluginAdapter

**Goal:** Slim down `pattern_core` dep surface (drop unused `reqwest`/`tokio-tungstenite`/`chrono`; feature-gate `loro`/`genai`/`toml`/`candle*`). Create the `pattern-plugin-sdk` workspace crate that re-exports plugin types from a slim `pattern_core` (no type duplication). Define `PluginProtocol` and `MemorySyncProtocol` IRPC services distinct from `PatternProtocol`, served on the **same daemon QUIC endpoint** via iroh's `Router` ALPN dispatch (`pattern/1` for TUI, `pattern-plugin/1` for plugins, `pattern-plugin-memory-sync/1` for the bidi memory delta sync). In-process plugin transport via direct trait dispatch (zero overhead — `Arc<dyn PluginConnection>` resolves to the in-process adapter's trait methods directly, no IRPC encoding); out-of-process via QUIC over loopback with iroh-node-identity allow-list. `McpPluginAdapter` wrapping standalone MCP servers as `PluginExtension` impls (in-process).

**Architecture:** Pattern's daemon currently uses `irpc::rpc::listen(endpoint, handler)` which is single-protocol. Phase 6 migrates to `iroh::protocol::Router::builder(endpoint).accept(b"pattern/1", ui_handler).accept(b"pattern-plugin/1", plugin_handler).accept(b"pattern-plugin-memory-sync/1", memory_sync_handler).spawn()`. One endpoint, one cert, three typed protocols (TUI, plugin main, plugin memory sync). Plugins authenticate at TLS layer via iroh node identity + an allow-list (key registered at install). Phase 6 introduces TWO plugin protocols: `PluginProtocol` (lifecycle + hooks + ports + host callbacks + db-poking memory ops) and `MemorySyncProtocol` (single bidi-streaming method for loro delta sync; feature-gated `memory-sync`). Splitting the bidi delta sync to its own ALPN keeps lifecycle isolated from long-lived stateful streams and lets memory-sync be feature-gated independently. `pattern_core` ships with feature flags: default = traits + types + error + hooks + capability + ports (lean); `memory` enables `loro`; `provider` enables `genai`; `embeddings-local` enables `candle*`+`hf-hub`+`tokenizers`; `sqlite` enables `rusqlite`. `pattern-plugin-sdk` depends on `pattern_core` with `default-features = false`. Plugin authors get hooks, capability, ports, plugin trait, hook tag catalog — without loro/genai/etc. Optional `memory-sync` feature on plugin SDK enables `LoroDoc` re-export + the bidi delta-sync surface on `PluginMemorySync`. `PluginMemorySync` is shaped like a slim `MemoryCache`: holds a `DashMap<BlockAddr, CachedBlock>`, impls `MemoryStore` directly, runs background tasks for outbound/inbound delta sync over IRPC. Most `MemoryStore` ops are local cache hits; only db-poking ops (search, create, list, delete, persist, archival) cross the wire. `McpPluginAdapter` wraps a single `McpServerConfig` from a manifest, runs in-process, reuses Phase 5's `McpClient` internally; adapter methods translate MCP tools to port `Call` ops and MCP resources to port `Subscribe` ops.

**Tech Stack:** `irpc 0.14` (existing), `iroh 0.95` (already a transitive dep via irpc), `iroh::protocol::Router` for ALPN dispatch, existing tokio/parking_lot. New workspace crate: `pattern-plugin-sdk`. New entrypoint `pattern_plugin_sdk::register_plugin(...)` for plugin authors.

**Scope:** 6 of 7 phases.

**Codebase verified:** 2026-04-27.

---

## Codebase verification findings

- ✓ `irpc 0.14` workspace dep (`Cargo.toml:160`); `iroh::protocol::Router::builder().accept(alpn, handler).spawn()` is the documented multi-protocol shape (see irpc-iroh examples `0rtt.rs:125-136`, `auth.rs:31-35`).
- ✓ Current single-protocol setup at `pattern_server/src/main.rs:153-168` uses `irpc::rpc::listen(endpoint, handler)`. Phase 6 replaces with `Router::builder()`.
- ✓ `Client::local(msg_tx)` already used for in-process tests (`server.rs:450`, `tests/...:2702-2826`). Plugin in-process transport reuses this.
- ✓ `pattern_core` direct dep audit (Cargo.toml + grep over src/):
  - **Drop entirely (declared, unused):** `reqwest`, `tokio-tungstenite`, `chrono` (one usage in `test_helpers.rs::Utc` swappable to `jiff::Timestamp::now()`).
  - **Feature-gate (used by core but not plugin SDK):**
    - `loro` 1.10 (heavy use in `memory/document.rs`) → behind `memory` feature
    - `genai` (used in `traits/provider_client.rs`, `error/core.rs::ProviderError`, `types/snapshot.rs::AdapterKind`) → behind `provider` feature
    - `toml` (one usage at `memory/document.rs:835`) → behind `memory` feature (alongside loro)
    - `candle-core/nn/transformers`, `hf-hub`, `tokenizers` → already optional; expose under `embeddings-local` feature
    - `rusqlite` → already optional under existing feature
  - **Keep in default (light, useful for plugin SDK):** `tokio`, `serde`, `serde_json`, `miette`, `thiserror`, `anyhow`, `tracing`, `metrics`, `async-trait`, `uuid`, `jiff`, `futures`, `parking_lot`, `dirs`, `secrecy`, `schemars`, `compact_str`, `smol_str` (transitive).
- ✓ `pattern_core::constellation` (the agent-grouping module at `crates/pattern_core/src/constellation.rs`, distinct from Phase 7's atproto Constellation client) imports zero feature-gated crates — verified via grep for `loro`/`genai`/`reqwest`/`candle`/`tungstenite`/`rusqlite`/`toml` against the file: zero matches. It stays in the default-feature build, no `#[cfg(feature = ...)]` gating needed.
- ✓ Other `pattern_core` modules with possible feature exposure verified as part of Task 1's grep audit: `traits/`, `types/`, `error/`, `capability.rs`, `hooks/` (Phase 2), `permission.rs`, `policy.rs`, `fronting.rs`, `commands.rs` (Phase 4) — all `default-features = false` clean.
- ✓ `keyring` workspace dep already present (`Cargo.toml:135-139`). Plugin keypair storage uses it.
- ✓ Existing `#[rpc_requests(message = ...)]` derive pattern at `pattern_server/src/protocol.rs:649-651`. `PluginProtocol` follows the same shape.
- ✓ `crates/pattern_plugin_sdk/` does NOT exist — clean slate.
- ✓ `Phase 3` Plugin trait surface (`PluginExtension`, `PluginHost`, `PluginContext`, `PortDeclaration`, `PluginError`) lives at `crates/pattern_core/src/traits/plugin/`. `Author::Plugin { plugin_id, partner_authority }` variant available from Phase 3. `PluginHost` trait method signatures match `PluginProtocol`'s host-callback variants from Task 3 — `RuntimePluginHost` (in `pattern_runtime`) and `IrpcPluginHost` (in `pattern_plugin_sdk`) are the two real impls.
- ✓ `pattern_core::hooks::*` module from Phase 2 — `HookEvent`, tag catalog, `cc_aliases::translate_cc`. Plugin SDK re-exports.
- ✓ `Port` trait + `PortRegistryImpl` from sandbox-io Phase 4-5. `McpPluginAdapter`'s `ports()` declarations register here.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-extensibility.AC6: IRPC transport and plugin SDK

- **v3-extensibility.AC6.1 Success:** IRPC-native plugin registers ports via `ports()` over IRPC; pattern records the plugin's declared ports and capabilities
- **v3-extensibility.AC6.2 Success:** Agent calls `ctx.port.call(plugin_port, method, payload)`; dispatched to plugin's port implementation over IRPC; response returned
- **v3-extensibility.AC6.3 Success:** Plugin calls back to Pattern via `PluginContext` accessors — `ctx.memory()` returns a `MemoryStore` impl that round-trips through `PluginProtocol`/`MemorySyncProtocol`, `ctx.send_message(...)` delivers to target agent's mailbox, `ctx.task_create(...)` adds to TaskList. *(Reinterpreted: design said "via PluginHost" — with the dropped trait, the equivalent is "via PluginContext + wire protocol".)*
- **v3-extensibility.AC6.4 Success:** Agent calls `ctx.port.subscribe(plugin_port, config)`; events stream from plugin to pattern via IRPC server-stream; delivered as system reminders
- **v3-extensibility.AC6.5 Success:** `McpPluginAdapter` wraps standalone MCP server as `PluginExtension`; MCP tools accessible as port calls; MCP resources as port subscriptions
- **v3-extensibility.AC6.6 Success:** Per-plugin cryptographic auth via iroh node identity (local) *(remote atproto-backed auth lands in Phase 7)*
- **v3-extensibility.AC6.7 Failure:** IRPC connection to plugin drops; plugin marked unhealthy; reconnection attempted; `PluginError::TransportLost` surfaced on next call
- **v3-extensibility.AC6.8 Edge:** `pattern-plugin-sdk` crate compiles with minimal dependencies; does not pull in `pattern_runtime` or `pattern_memory`
- **v3-extensibility.AC6.9 Edge:** In-process plugin dispatch used by CC and MCP adapters verifiably has zero network overhead. *(Reinterpreted: design plan said "IRPC in-process mode (tokio mpsc)" — Task 4 implements direct trait dispatch instead, which is strictly stronger: no encoding, no channel hop, just a vtable call. Same observable property — zero network overhead — verified at the trait-dispatch layer.)*

### v3-extensibility.AC3: CC plugin adapter (final cases)

- **v3-extensibility.AC3.8 Failure:** CC plugin subprocess crashes; `PluginError::ProcessDied` surfaced; plugin marked unhealthy in registry *(landed here because process supervision belongs to Phase 6's transport machinery)*

---

## Tasks

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: `pattern_core` dep audit + `Port::library()` SmolStr swap

**This task does two related things in one atomic commit:** the feature-gate refactor of `pattern_core` deps AND a small Port trait change that the plugin protocol depends on (port library text crosses the wire as a SmolStr-shaped string).

#### 1a. `Port::library()` return type swap

**Files:**
- Modify: `crates/pattern_core/src/traits/port.rs` — `fn library(&self) -> Option<&'static str>` → `fn library(&self) -> Option<smol_str::SmolStr>`. Default impl returns `None` unchanged.
- Modify: `crates/pattern_runtime/src/ports/http.rs` and any other in-tree Port impls — wrap existing `&'static str` returns with `SmolStr::new_static(...)`.
- Modify: any test fixture / dummy Port impls (e.g., the docstring example).

`SmolStr::new_static("...")` is zero-alloc for compile-time strings (just a tagged pointer); cheap `Clone`; runtime-constructable from any `&str`/`String`. Eliminates the `Box::leak` footgun for plugins that need runtime-built libraries. Wire format trivially serializes (it's just a string).

This is a small, mechanical change but it lands in this phase because Phase 6's `WirePortDeclaration` carries `library: Option<SmolStr>` over the wire — same type both sides of the boundary.

#### 1b. `pattern_core` dep audit — drop unused, feature-gate heavy

**Verifies:** AC6.8 (foundation).

**Files:**
- Modify: `crates/pattern_core/Cargo.toml` — drop unused deps; introduce features.
- Modify: `crates/pattern_core/src/test_helpers.rs:9` — swap `chrono::Utc` for `jiff::Timestamp::now()`.
- Modify: callers in `crates/pattern_runtime/Cargo.toml`, `crates/pattern_memory/Cargo.toml`, `crates/pattern_db/Cargo.toml`, `crates/pattern_provider/Cargo.toml`, `crates/pattern_server/Cargo.toml`, `crates/pattern_cli/Cargo.toml` — enable the appropriate features on `pattern-core`.

**Implementation:**

`pattern_core/Cargo.toml`:

```toml
[package]
name = "pattern-core"

[dependencies]
# Lean default deps (used everywhere, light)
tokio = { workspace = true }
tokio-stream = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
miette = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
metrics = { workspace = true }
async-trait = { workspace = true }
uuid = { workspace = true }
jiff = { workspace = true }
futures = { workspace = true }
parking_lot = { workspace = true }
dirs = { workspace = true }
secrecy = { workspace = true }
schemars = { workspace = true }
compact_str = { version = "0.9.0", features = ["serde", "markup", "smallvec"] }
smol_str = { workspace = true }
globset = { workspace = true }    # for hooks::HookFilter (Phase 2)

# Feature-gated deps
loro = { version = "1.10", features = ["counter"], optional = true }
toml = { workspace = true, optional = true }
genai = { workspace = true, optional = true }
candle-core = { version = "0.9", optional = true }
candle-nn = { version = "0.9", optional = true }
candle-transformers = { version = "0.9", optional = true }
hf-hub = { version = "0.4", default-features = false, features = ["rustls-tls", "tokio"], optional = true }
tokenizers = { version = "0.21", optional = true }
rusqlite = { version = "0.39", optional = true }
reqwest-middleware = { version = "0.4", optional = true }
http = { version = "1.1", optional = true }

# DROPPED (declared but unused in pattern_core/src):
# - reqwest, tokio-tungstenite, chrono

[features]
default = []
memory = ["dep:loro", "dep:toml"]
provider = ["dep:genai"]
embeddings-local = ["dep:candle-core", "dep:candle-nn", "dep:candle-transformers", "dep:hf-hub", "dep:tokenizers"]
sqlite = ["dep:rusqlite"]
http-extras = ["dep:reqwest-middleware", "dep:http"]
test-support = []
```

Update consumer `Cargo.toml`s:
- `pattern_memory`: `pattern-core = { path = "../pattern_core", features = ["memory", "sqlite"] }` (memory uses loro; pattern_db likewise pulls sqlite).
- `pattern_runtime`: `pattern-core = { path = "../pattern_core", features = ["memory", "provider"] }`.
- `pattern_provider`: `pattern-core = { path = "../pattern_core", features = ["provider", "embeddings-local"] }`.
- `pattern_db`: `pattern-core = { path = "../pattern_core", features = ["sqlite"] }`.
- `pattern_server`, `pattern_cli`: depend with whatever superset is needed for daemon/TUI use.

The `chrono::Utc::now()` swap in `test_helpers.rs`:

```rust
// before:
let ts = chrono::Utc::now();
// after:
let ts = jiff::Timestamp::now();
```

If `chrono::Utc` is used elsewhere in `pattern_core` (run a final grep), swap each usage. Then remove `chrono` from `[dependencies]` entirely.

Compile-fence each feature:

```rust
// In types/snapshot.rs (genai usage):
#[cfg(feature = "provider")]
use genai::adapter::AdapterKind;

// In memory/document.rs (loro usage):
#[cfg(feature = "memory")]
use loro::{ ... };
```

The full `memory/` and `traits/provider_client.rs` modules become `#[cfg(feature = "memory")]` / `#[cfg(feature = "provider")]` gated, since their entire surface depends on the heavy dep.

**Testing:**
Tests must verify each AC listed above:
- AC6.8 foundation: `cargo build -p pattern-core --no-default-features` builds clean. No loro/genai/candle in the resulting dep tree.
- `cargo build -p pattern-core --features=memory` adds loro+toml.
- `cargo build -p pattern-core --features=provider` adds genai.
- `cargo build --workspace` (with consumer crates' default feature flags) is byte-equivalent to today (no functional regression).

**Verification:**
```bash
cargo build -p pattern-core --no-default-features
cargo tree -p pattern-core --no-default-features --depth 1   # confirm slim graph
cargo nextest run --workspace
```

**Commit:** `[meta] [pattern-core] [pattern-runtime] swap Port::library to Option<SmolStr> + feature-gate heavy core deps`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Create `crates/pattern_plugin_sdk/` workspace crate

**Verifies:** AC6.8.

**Files:**
- Create: `crates/pattern_plugin_sdk/Cargo.toml`.
- Create: `crates/pattern_plugin_sdk/src/lib.rs`.
- Create: `crates/pattern_plugin_sdk/README.md` — crate purpose, opt-in features.
- Modify: workspace `Cargo.toml` — add `crates/pattern_plugin_sdk` to `[workspace].members`.

**Implementation:**

```toml
# crates/pattern_plugin_sdk/Cargo.toml
[package]
name = "pattern-plugin-sdk"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
description = "SDK for authoring Pattern plugins (in-process or out-of-process via IRPC)"

[dependencies]
# Slim core: no loro, no genai, no candle.
pattern-core = { path = "../pattern_core", default-features = false }

# IRPC for the plugin transport
irpc = { workspace = true }
iroh = { workspace = true }

# Always-needed plumbing
tokio = { workspace = true, features = ["rt", "macros", "sync"] }
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
smol_str = { workspace = true }

[features]
default = []
# Opt-in: enables LoroDoc re-export + PluginMemorySync::{subscribe, apply_delta}
# loro-delta surface on top of the always-available MemoryStore impl.
memory-sync = ["pattern-core/memory"]
```

`lib.rs`:

```rust
//! Pattern plugin SDK.
//!
//! Authors implement [`PluginExtension`] and call [`register_plugin`] from
//! their plugin's main(). The SDK handles transport wiring (direct trait
//! dispatch in-process; IRPC over iroh QUIC out-of-process), serialization,
//! and host callback dispatch via `PluginContext` accessors.
//!
//! ## Minimum example
//!
//! ```rust,no_run
//! use pattern_plugin_sdk::{PluginExtension, register_plugin};
//!
//! #[derive(Debug, Default)]
//! struct MyPlugin;
//!
//! impl PluginExtension for MyPlugin {
//!     fn ports(&self) -> Vec<pattern_plugin_sdk::PortDeclaration> { Vec::new() }
//! }
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     register_plugin(MyPlugin::default()).await?;
//!     Ok(())
//! }
//! ```

// Re-exports from pattern_core (slim).
pub use pattern_core::traits::plugin::{
    PluginContext, PluginError, PluginExtension, PluginHost, PortDeclaration,
};
pub use pattern_core::hooks::{
    HookEvent, HookEventMetadata, HookFilter, HookResponse, HookSemantics,
    cc_aliases, payloads, tags,
};
pub use pattern_core::capability::{CapabilityFlag, CapabilitySet, EffectCategory};
pub use pattern_core::traits::port::{Port, PortCapabilities, PortError, PortEvent, PortMetadata};
pub use pattern_core::traits::memory_store::MemoryStore;
pub use pattern_core::types::port::PortId;
pub use pattern_core::types::origin::Author;

// Optional memory-sync surface.
#[cfg(feature = "memory-sync")]
pub use pattern_core::memory::LoroDoc;

// PluginMemorySync — concrete struct that impls MemoryStore over IRPC.
// Always available (sub-task of Task 5); the loro-delta-streaming methods
// (subscribe / apply_delta) are gated behind `memory-sync`.
mod memory_sync;
pub use memory_sync::PluginMemorySync;

mod registration;
mod transport;

pub use registration::register_plugin;
```

The `register_plugin` entry point reads env-var configuration provided by the runtime (`PATTERN_PLUGIN_TRANSPORT={inproc|quic}`, `PATTERN_PLUGIN_NODE_KEY=<base64>`, `PATTERN_PLUGIN_RUNTIME_ADDR=<addr>`, etc.) and starts the appropriate IRPC server. For in-process mode (used by CcPluginAdapter + McpPluginAdapter), `register_plugin` is bypassed — the adapters construct the SDK's surface directly. `register_plugin` is for out-of-process plugins.

**Testing:**
- Build the empty SDK with default features; assert dep graph minimal: `cargo tree -p pattern-plugin-sdk` does NOT contain `loro`, `genai`, `candle`, `tokio-tungstenite`, `reqwest`, `rusqlite`. Confirms AC6.8.
- Build with `--features memory-sync`; assert `loro` appears.

**Verification:**
Run: `cargo check -p pattern-plugin-sdk` and `cargo tree -p pattern-plugin-sdk`.

**Commit:** `[meta] [pattern-plugin-sdk] new workspace crate — slim plugin author SDK`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->

<!-- START_TASK_3 -->
### Task 3: Plugin wire protocols — `PluginProtocol` + `MemorySyncProtocol`

**Verifies:** AC6.1, AC6.3 foundation; AC6.4 (subscribe streams); AC6.6 (auth-shaped at the protocol layer).

**Files:**
- Create: `crates/pattern_core/src/traits/plugin/wire.rs` — wire types shared between runtime and plugin SDK (`WirePluginContext`, `WirePortDeclaration`, `WireJson`, `SnapshotPayload`, `DeltaPayload`, etc.).
- Create: `crates/pattern_runtime/src/plugin/protocol.rs` — defines `PluginProtocol` and `MemorySyncProtocol` enums via `#[rpc_requests]`.

**Implementation:**

**Two protocols on two ALPNs**, multiplexed on the daemon's QUIC endpoint via `iroh::protocol::Router`:

- **`pattern-plugin/1`** — `PluginProtocol`. Request-response shaped, with a few server-streams. Lifecycle, hooks, port operations, host callbacks, db-poking memory ops.
- **`pattern-plugin-memory-sync/1`** — `MemorySyncProtocol`. Single bidirectional-streaming method for loro delta sync. Feature-gated (`memory-sync` SDK feature). Plugins that don't enable the feature don't register the ALPN.

Reasoning for the split: bidi-streaming has different lifecycle (long-lived, stateful, drop-cancellable) than the unary majority; independent versioning; different concurrency profile; one ALPN-worth of methods is unreachable for plugins that don't opt into memory sync.

**Postcard incompatibility with `serde_json::Value`.** `Value` cannot serialize through postcard (no static schema). All wire variants that would carry arbitrary JSON use a `WireJson` newtype (text-encoded JSON, decoded on demand):

```rust
/// Postcard-friendly wrapper for arbitrary JSON. Round-trips through the
/// wire as a String; decoded on demand via `parse()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireJson(pub String);

impl WireJson {
    pub fn from_value(v: &serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(Self(serde_json::to_string(v)?))
    }
    pub fn parse(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.0)
    }
}
```

**Chunked payloads** for snapshots and deltas — type-level shape baked in v1 even though v1 only emits the `Inline` variant; receivers MUST handle both shapes so v2+ can flip emission to `Chunked` without bumping the wire version. (irpc has no built-in versioning; bake forward-compat into the type.)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SnapshotPayload {
    /// Single-frame snapshot. v1 always emits this.
    Inline { bytes: Vec<u8> },
    /// Multi-frame, sequence-addressed within `chunk_id`. `final_chunk = true`
    /// completes the snapshot. Receivers in v1 buffer + assemble; emitters
    /// in v1 do not produce this. v2+ flips emission without protocol bump.
    Chunked { chunk_id: SmolStr, seq: u32, final_chunk: bool, bytes: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DeltaPayload {
    Inline { bytes: Vec<u8> },
    Chunked { chunk_id: SmolStr, seq: u32, final_chunk: bool, bytes: Vec<u8> },
}
```

**Main protocol:**

```rust
use irpc::rpc_requests;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use pattern_core::traits::plugin::wire::*;

#[rpc_requests(message = PluginMessage)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PluginProtocol {
    // ===== Runtime → Plugin (PluginExtension surface) =====
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    OnInstall(WirePluginContext),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    OnEnable(WirePluginContext),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    OnDisable(WirePluginContext),
    #[rpc(tx = oneshot::Sender<Vec<WirePortDeclaration>>)]
    DeclarePorts,
    #[rpc(tx = oneshot::Sender<Option<SmolStr>>)]
    GetLibrary,
    #[rpc(tx = oneshot::Sender<()>)]
    OnHookEvent(WireHookEvent),
    #[rpc(tx = oneshot::Sender<WireHookResponse>)]
    OnHookEventBlocking(WireHookEvent),
    #[rpc(tx = oneshot::Sender<Result<WireJson, WirePortError>>)]
    PortCall(WirePortCallRequest),
    #[rpc(tx = mpsc::Sender<WirePortEvent>)]
    PortSubscribe(WirePortSubscribeRequest),

    // ===== Plugin → Runtime (port lifecycle) =====
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    RegisterPort(WirePortDeclaration),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    UnregisterPort { port_id: PortId },
    #[rpc(tx = oneshot::Sender<()>)]
    PortStatusChanged { port_id: PortId, status: WirePortStatus },

    // ===== Plugin → Runtime (host callbacks) =====
    #[rpc(tx = oneshot::Sender<Vec<WireSearchResult>>)]
    HostSearch(WireSearchQuery),
    #[rpc(tx = oneshot::Sender<Result<MessageId, WirePluginError>>)]
    HostSendMessage(WireHostMessage),
    #[rpc(tx = oneshot::Sender<Result<TaskItemId, WirePluginError>>)]
    HostTaskCreate(WireTaskCreate),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    HostTaskTransition(WireTaskTransition),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    HostTaskLink(WireTaskLink),
    #[rpc(tx = oneshot::Sender<Vec<WireTaskItem>>)]
    HostTaskQuery(WireTaskQuery),
    #[rpc(tx = oneshot::Sender<Result<WireSkillInvocation, WirePluginError>>)]
    HostSkillInvoke(WireSkillInvoke),

    // ===== Plugin → Runtime (db-poking memory ops) =====
    #[rpc(tx = oneshot::Sender<Result<BlockMetadata, WireMemoryError>>)]
    MemoryCreateBlock(BlockCreate),
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    MemoryDeleteBlock { addr: BlockAddr },
    #[rpc(tx = oneshot::Sender<Vec<WireSearchResult>>)]
    MemorySearch(SearchQuery),
    #[rpc(tx = oneshot::Sender<Vec<BlockMetadata>>)]
    MemoryListBlocks(BlockFilter),
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    MemoryPersist { addr: BlockAddr },
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    MemoryUpdateMetadata { addr: BlockAddr, patch: BlockMetadataPatch },
    #[rpc(tx = oneshot::Sender<Result<bool, WireMemoryError>>)]
    MemoryUndoRedo { addr: BlockAddr, op: UndoRedoOp },
    #[rpc(tx = oneshot::Sender<Result<Option<BlockMetadata>, WireMemoryError>>)]
    MemoryGetSharedBlock { owner: SmolStr, label: SmolStr },
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    MemoryInsertArchival(WireArchivalEntry),
    #[rpc(tx = oneshot::Sender<Vec<WireArchivalEntry>>)]
    MemorySearchArchival(SearchQuery),
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    MemoryDeleteArchival { id: SmolStr },
}
```

**Memory sync protocol** (separate ALPN, bidi-streaming):

```rust
#[rpc_requests(message = MemorySyncMessage)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemorySyncProtocol {
    /// Open a bidirectional delta-sync session for blocks matching `filter`.
    /// Plugin sends local edits via `rx`, runtime sends events (BlockAvailable,
    /// Delta, BlockGone) via `tx`. Drop on either side closes the session.
    #[rpc(tx = mpsc::Sender<WireMemoryEvent>, rx = mpsc::Receiver<WireMemoryEdit>)]
    Sync(BlockFilter),
}
```

**Wire types** in `pattern_core::traits::plugin::wire`:

```rust
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::types::port::{PortId, PortMetadata, PortCapabilities};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePluginContext {
    pub plugin_id: SmolStr,
    pub plugin_root: std::path::PathBuf,
    pub user_config: WireJson,
    pub effective_capabilities: CapabilitySet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortDeclaration {
    pub id: PortId,
    pub metadata: PortMetadata,
    pub capabilities: PortCapabilities,
    pub library: Option<SmolStr>,        // SmolStr per Phase 6 Task 1 swap
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortCallRequest {
    pub port_id: PortId,
    pub method: SmolStr,
    pub payload: WireJson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortSubscribeRequest {
    pub port_id: PortId,
    pub config: WireJson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortEvent {
    pub port_id: PortId,
    pub payload: WireJson,
    pub at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WirePortStatus {
    Healthy,
    Unavailable { reason: SmolStr },
    RateLimited { retry_after_secs: u32 },
    Reconnecting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireHookEvent {
    pub tag: SmolStr,
    pub payload: WireJson,
    pub metadata: WireHookEventMetadata,
    pub semantics: HookSemantics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireHookResponse {
    Continue,
    Block { reason: SmolStr },
    Modify(WireJson),
}

/// Block addressing — natural address used by every wire op. NO internal
/// `block_id: String` (uuid) ever crosses the wire; runtime resolves
/// `(agent_id, label, scope)` → block_id server-side via MemoryCache index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockAddr {
    pub agent_id: SmolStr,
    pub label: SmolStr,
    pub scope: MemoryScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireMemoryEvent {
    BlockAvailable { addr: BlockAddr, metadata: BlockMetadata, snapshot: SnapshotPayload },
    Delta { addr: BlockAddr, payload: DeltaPayload },
    BlockGone { addr: BlockAddr, reason: BlockGoneReason },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireMemoryEdit {
    /// Plugin pushed a local edit. Runtime applies to its loro doc, fires
    /// downstream subscribers, persists per scope wrapper's policy.
    Delta { addr: BlockAddr, payload: DeltaPayload },
}

// ... WireSearchQuery, WireSearchResult, WireHostMessage, WireTaskCreate,
//     WireTaskTransition, WireTaskLink, WireTaskQuery, WireTaskItem,
//     WireSkillInvoke, WireSkillInvocation, WireArchivalEntry,
//     WirePluginError, WirePortError, WireMemoryError, BlockGoneReason
```

**Server-side dispatch.** Runtime-side handlers for plugin → runtime variants dispatch into the **scoped MemoryStore wrapper** (`pattern_memory::scope::MemoryScope<S>`), NOT directly to `MemoryCache`. The scope wrapper enforces the plugin's declared `MemoryScope` from its manifest (`requires { memory { scope ... mode ... } }`) before reaching the underlying cache + db. Permission broker is also consulted per-call for any operation that would otherwise need approval per the policy rules.

For `MemorySync(BlockFilter)`, runtime resolves the filter into a set of subscribed blocks, sends initial `BlockAvailable` events with snapshots, then keeps the bidi stream open: every loro change on watched blocks emits a `Delta` event outbound, every inbound `WireMemoryEdit::Delta` is applied to the runtime's loro doc + triggers the existing subscriber/persist machinery.

**Postcard 16 MiB limit acknowledgment.** Snapshots and deltas use `SnapshotPayload`/`DeltaPayload` types with both `Inline` and `Chunked` variants exposed. v1 emits only `Inline` (Pattern's current memory blocks are well below 16 MiB). v2+ can ship `Chunked` emission without protocol churn — the wire shape already accommodates it.

**Testing:**
Tests must verify each AC listed above:
- AC6.1: Round-trip every `PluginProtocol` variant through postcard; assert decode equality.
- AC6.4: Open a `MemorySync` bidi stream against a fixture runtime; observe initial `BlockAvailable`, push a `Delta` from the plugin side, observe runtime emits the corresponding `Delta` back (to other subscribers).
- Chunked-payload forward-compat: hand-craft a `Chunked` `SnapshotPayload` and assert the receiver assembles correctly even though v1 emitters never produce it.

**Verification:**
Run: `cargo check -p pattern-runtime` and `cargo test -p pattern-runtime plugin::protocol::roundtrip`.

**Commit:** `[pattern-core] [pattern-runtime] add PluginProtocol + MemorySyncProtocol IRPC services + wire types`
<!-- END_TASK_3 -->

**Testing:**
- Compile-only test: every request variant round-trips through postcard (`postcard::to_stdvec` + `from_bytes`).
- The `pattern-plugin-sdk` re-exports the wire types so plugin author code sees `pattern_plugin_sdk::wire::WirePluginContext` etc.

**Verification:**
Run: `cargo check -p pattern-runtime` and `cargo test -p pattern-runtime plugin::protocol::roundtrip`.

**Commit:** `[pattern-core] [pattern-runtime] add PluginProtocol IRPC service + wire types`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: In-process plugin transport (direct trait dispatch) — adapt CC + MCP adapters

**Verifies:** AC6.9, AC3.8 (process-died for in-process is N/A; out-of-process surfaces it in Task 5).

**Files:**
- Create: `crates/pattern_runtime/src/plugin/transport/inprocess.rs` — `InProcessPluginTransport`.
- Modify: `crates/pattern_runtime/src/plugin/cc_adapter.rs` — drive through the in-process transport rather than direct trait dispatch (so the runtime code path is uniform).
- Modify: `crates/pattern_runtime/src/plugin/registry.rs` — rename `LoadedPlugin.extension` (`Arc<dyn PluginExtension>`) → `LoadedPlugin.connection` (`Arc<dyn PluginConnection>`), where `PluginConnection` is the transport-agnostic interface with in-process and out-of-process impls. The `host: Option<Arc<dyn PluginHost>>` field stays as defined in Phase 3 — Phase 6 wires `IrpcPluginHost` into `host` for native plugins (CC adapter still sets None).

**Implementation:**

A new `PluginConnection` trait abstracts over transport:

```rust
#[async_trait::async_trait]
pub trait PluginConnection: Send + Sync + std::fmt::Debug {
    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError>;
    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError>;
    async fn on_disable(&self, ctx: &PluginContext) -> Result<(), PluginError>;
    async fn declare_ports(&self) -> Result<Vec<PortDeclaration>, PluginError>;
    async fn library(&self) -> Result<Option<String>, PluginError>;
    async fn on_event(&self, event: HookEvent) -> Result<Option<HookResponse>, PluginError>;
    async fn port_call(&self, port: PortId, method: &str, payload: serde_json::Value) -> Result<serde_json::Value, PluginError>;
    async fn port_subscribe(&self, port: PortId, config: serde_json::Value) -> Result<futures::stream::BoxStream<'static, PortEvent>, PluginError>;

    fn health(&self) -> PluginHealth;  // Healthy | Unhealthy { reason }
}
```

`InProcessPluginConnection` wraps an `Arc<dyn PluginExtension>` directly (no IRPC needed for in-process — call methods through the trait object):

```rust
#[derive(Debug)]
pub struct InProcessPluginConnection {
    extension: Arc<dyn PluginExtension>,
    plugin_id: SmolStr,
}

#[async_trait::async_trait]
impl PluginConnection for InProcessPluginConnection {
    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        self.extension.on_install(ctx).await
    }
    // ... pass-through ...

    async fn port_subscribe(&self, port: PortId, config: serde_json::Value)
        -> Result<futures::stream::BoxStream<'static, PortEvent>, PluginError>
    {
        // Plugin's port impl returns a BoxStream; passthrough.
    }

    fn health(&self) -> PluginHealth { PluginHealth::Healthy }
}
```

This is the trivial in-process path: no serialization, no channels. Phase 6 still goes through the `PluginConnection` interface so the runtime treats both transports uniformly. Per AC6.9, the in-process variant is verifiably zero-overhead — direct method dispatch.

CC adapter and MCP plugin adapter (Task 6) wrap with `InProcessPluginConnection` at registry insert time; the `LoadedPlugin.connection` field (renamed from Phase 3's `extension`) holds the transport-agnostic interface. The Phase 3 `host: Option<Arc<dyn PluginHost>>` field stays — Phase 6 populates it with `IrpcPluginHost` for out-of-process native plugins (CC stays `None`). Native plugin's `PluginContext.host` field is set from `LoadedPlugin.host` at session-bind time.

**Testing:**
Tests must verify each AC listed above:
- AC6.9: A microbenchmark or `criterion` test asserts the in-process round-trip latency is sub-microsecond (or just confirm the trait dispatch compiles to a vtable call). Pragmatic test: instrument `InProcessPluginConnection::port_call` to confirm it doesn't hit any channel/serialization codepath.
- Existing Phase 3/Phase 5 CC adapter / McpPluginAdapter integration tests pass unchanged — the connection abstraction is transparent to test fixtures.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::`.

**Commit:** `[pattern-runtime] add PluginConnection trait + InProcess transport`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Out-of-process plugin transport (QUIC) + iroh-Router migration + auth

**Verifies:** AC6.1, AC6.2, AC6.3, AC6.4, AC6.6 (local), AC6.7, AC3.8.

**Files:**
- Modify: `crates/pattern_server/src/main.rs:153-168` — replace `irpc::rpc::listen(endpoint, handler)` with `iroh::protocol::Router::builder(endpoint).accept(b"pattern/1", ...).accept(b"pattern-plugin/1", ...).accept(b"pattern-plugin-memory-sync/1", ...).spawn()`.
- Create: `crates/pattern_runtime/src/plugin/transport/out_of_process.rs` — `OutOfProcessPluginConnection` + child-process spawn + auth.
- Create: `crates/pattern_runtime/src/plugin/auth.rs` — iroh node identity allow-list + per-plugin keypair generation at install.
- Modify: `crates/pattern_runtime/src/plugin/registry.rs` — install path generates plugin keypair, records public key in registry KDL, builds `OutOfProcessPluginConnection` for native plugins (where `manifest.transport == TransportPreference::OutOfProcess`).

**Implementation:**

**Daemon endpoint migration.** Swap single-protocol `listen` for `Router::builder()`:

```rust
use iroh::protocol::Router;
use irpc_iroh::IrohProtocol;

// pattern_server::main
let endpoint = irpc::util::make_server_endpoint(bind_addr)?;
let local = handle.client.as_local().expect("freshly-spawned server client must be local");

let ui_handler = PatternProtocol::remote_handler(local.clone());
let plugin_handler = PluginProtocol::remote_handler(/* runtime's plugin host impl */);

let _router = Router::builder(endpoint)
    .accept(b"pattern/1",                       IrohProtocol::new(ui_handler))
    .accept(b"pattern-plugin/1",                IrohProtocol::new(plugin_handler))
    .accept(b"pattern-plugin-memory-sync/1",    IrohProtocol::new(memory_sync_handler))
    .spawn();
```

**Auth: iroh node identity allow-list.** Per-plugin keypair generated at `PluginRegistry::install` time; public key stored in the registry KDL alongside the install record:

```kdl
plugin "github-bridge" {
    source "git+https://example.com/github-bridge"
    transport "out-of-process"
    node-key "01a3f4b2..."   // base64 z-base32 iroh node id
    installed-at "2026-04-27T14:32:00Z"
}
```

The plugin's keypair private half is stored in the `keyring` (workspace dep) under the entry `pattern.plugin.<plugin-id>.priv`. Registry persists only the public key.

At connection-accept time inside `PluginProtocol::remote_handler`, the daemon retrieves the connecting peer's iroh node id via the iroh endpoint's connection metadata. If the node id matches a registered plugin's public key, the connection is accepted; otherwise rejected with a `tracing::warn` log.

**Out-of-process plugin lifecycle.** The runtime owns the plugin process:

```rust
pub struct OutOfProcessPluginConnection {
    plugin_id: SmolStr,
    process: parking_lot::Mutex<Option<tokio::process::Child>>,
    irpc_client: irpc::Client<PluginProtocol>,
    health: parking_lot::RwLock<PluginHealth>,
}

impl OutOfProcessPluginConnection {
    pub async fn spawn(plugin_id: SmolStr, plugin_root: &Path, manifest: &PluginManifest, daemon_addr: SocketAddr, runtime_pubkey: iroh::PublicKey) -> Result<Self, PluginError> {
        // 1. Read keypair from keyring; export private+public to env vars for the child.
        // 2. Spawn the plugin binary (path declared in manifest.transport.out_of_process.binary).
        //    Child env: PATTERN_PLUGIN_TRANSPORT=quic, PATTERN_PLUGIN_RUNTIME_ADDR=<addr>,
        //               PATTERN_PLUGIN_NODE_KEY=<priv-key>, PATTERN_RUNTIME_PUBKEY=<runtime-pub>.
        // 3. Wait briefly for child to connect back via IRPC (handshake on PluginProtocol).
        // 4. Return connection.
    }
}
```

`PluginConnection` impl for out-of-process delegates each method to an IRPC call:

```rust
#[async_trait::async_trait]
impl PluginConnection for OutOfProcessPluginConnection {
    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let wire_ctx: WirePluginContext = ctx.into();
        match self.irpc_client.rpc(OnInstall(wire_ctx)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.into()),
            Err(irpc::Error::ConnectionLost) => {
                self.mark_unhealthy("transport lost during on_install");
                Err(PluginError::TransportLost { plugin_id: self.plugin_id.clone(), message: "connection dropped".into() })
            }
            Err(e) => Err(PluginError::TransportLost { plugin_id: self.plugin_id.clone(), message: e.to_string() }),
        }
    }
    // ... rest of the surface ...

    fn health(&self) -> PluginHealth { self.health.read().clone() }
}
```

`OutOfProcessPluginConnection` also installs a process-supervisor task that observes the child's exit:

```rust
async fn supervise_child(plugin_id: SmolStr, mut child: tokio::process::Child, conn: Weak<OutOfProcessPluginConnection>) {
    let exit = child.wait().await;
    if let Some(conn) = conn.upgrade() {
        conn.mark_unhealthy(format!("child process exited: {exit:?}"));
        // Optionally trigger reconnect on next call (Phase 6 ships fail-and-surface;
        // auto-reconnect is a follow-up).
    }
}
```

This satisfies AC3.8 (CC subprocess crash → ProcessDied error surfaced) for out-of-process *and* for CC adapters that use subprocesses for command hooks; CC adapter is in-process so its process supervision happens at the ProcessManager level (per Phase 4 Task 5), which already surfaces shell exits as hook events.

**Testing:**
Tests must verify each AC listed above:
- AC6.1: Spawn a fixture plugin binary that declares one port; assert `LoadedPlugin.connection.declare_ports()` returns the expected `WirePortDeclaration`.
- AC6.2: Same fixture plugin handles a port call; assert `connection.port_call(...)` round-trips correctly.
- AC6.3: Fixture plugin's `on_install` calls `HostReadMemory` on the runtime; runtime returns the block content; plugin observes it. The plugin host implementation lives in `pattern_runtime`; verify that.
- AC6.4: Fixture plugin port subscription emits `WirePortEvent` items; assert the runtime drains them and surfaces as `MessageAttachment::PortEvent` in segment 2 (existing path from sandbox-io).
- AC6.6 local: Spawn fixture plugin; verify the daemon's allow-list contains the plugin's pubkey; spawn an unauthorized peer (different iroh node id) — assert connection is rejected.
- AC6.7: Kill the fixture plugin process mid-test; assert next `connection.port_call(...)` returns `PluginError::TransportLost`; assert `connection.health()` returns `Unhealthy`.
- AC3.8: Same as AC6.7 — process supervisor surfaces `ProcessDied` (variant flavor).

The fixture plugin lives at `crates/pattern_runtime/tests/fixtures/plugins/oop-fixture/` — a small Rust binary in a separate `Cargo.toml` that depends on `pattern-plugin-sdk`. Built as part of test setup via `cargo build --manifest-path tests/fixtures/plugins/oop-fixture/Cargo.toml` in `build.rs` or test setup helper.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::transport::out_of_process`.

**Commit:** `[pattern-server] [pattern-runtime] iroh Router migration + out-of-process plugin transport + iroh-id auth`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (task 6) -->

<!-- START_TASK_6 -->
### Task 6: `McpPluginAdapter` — wrap standalone MCP server as `PluginExtension`

**Verifies:** AC6.5.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/mcp_adapter.rs` — `McpPluginAdapter` impl.

**Implementation:**

`McpPluginAdapter` wraps a single `McpServerConfig` (from Phase 4's CC `.mcp.json` parser, or directly authored in a Pattern-native plugin manifest's `mcp_servers` block) as a `PluginExtension`. Reuses Phase 5's `McpClient` internally.

```rust
use std::sync::Arc;
use async_trait::async_trait;

use pattern_core::traits::plugin::{PluginContext, PluginError, PluginExtension, PortDeclaration};
use pattern_core::traits::port::{Port, PortCapabilities, PortError, PortEvent, PortMetadata};
use pattern_core::types::port::PortId;
use pattern_core::mcp::McpServerConfig;
use crate::mcp::{McpClient, McpError};

#[derive(Debug)]
pub struct McpPluginAdapter {
    plugin_id: smol_str::SmolStr,
    config: McpServerConfig,
    state: parking_lot::RwLock<AdapterState>,
}

#[derive(Debug, Default)]
struct AdapterState {
    client: Option<Arc<McpClient>>,
}

impl McpPluginAdapter {
    pub fn wrap(plugin_id: smol_str::SmolStr, config: McpServerConfig) -> Arc<Self> {
        Arc::new(Self { plugin_id, config, state: Default::default() })
    }
}

#[async_trait]
impl PluginExtension for McpPluginAdapter {
    fn ports(&self) -> Vec<PortDeclaration> {
        // One port per adapter: "mcp:<server>". Tools become call methods,
        // resources become subscribe topics.
        vec![PortDeclaration {
            id: PortId(format!("mcp:{}", self.config.name).into()),
            description: format!("MCP server bridge: {}", self.config.name),
            library: None,
        }]
    }

    async fn on_enable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        let client = McpClient::connect(&self.config).await
            .map_err(|e| PluginError::InstallFailed {
                plugin_id: self.plugin_id.clone(),
                message: format!("MCP connect: {e}"),
            })?;
        self.state.write().client = Some(Arc::new(client));
        Ok(())
    }

    async fn on_disable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        let client = self.state.write().client.take();
        if let Some(c) = client {
            // Best-effort disconnect.
            if let Some(c) = Arc::try_unwrap(c).ok() {
                let _ = c.disconnect().await;
            }
        }
        Ok(())
    }
}

/// Port impl for the adapter — runtime's port registry calls this for `mcp:<server>` port.
#[derive(Debug)]
pub struct McpPluginPort {
    adapter: Arc<McpPluginAdapter>,
}

#[async_trait]
impl Port for McpPluginPort {
    fn id(&self) -> &PortId { /* ... */ }

    fn metadata(&self) -> PortMetadata {
        PortMetadata { description: format!("MCP server: {}", self.adapter.config.name), tags: vec![] }
    }

    fn capabilities(&self) -> PortCapabilities {
        PortCapabilities::default().with_callable(true).with_subscribable(false)  // resources later
    }

    async fn call(&self, method: &str, payload: serde_json::Value) -> Result<serde_json::Value, PortError> {
        let client = self.adapter.state.read().client.clone()
            .ok_or_else(|| PortError::Unavailable { reason: "MCP plugin not enabled".into() })?;
        client.call_tool(method, payload).await
            .map_err(|e| match e {
                McpError::ServerUnavailable { .. } => PortError::Unavailable { reason: e.to_string() },
                _ => PortError::Other(e.to_string()),
            })
    }

    async fn subscribe(&self, _config: serde_json::Value) -> Result<futures::stream::BoxStream<'static, PortEvent>, PortError> {
        Err(PortError::NotSupported {
            method: "subscribe".into(),
            reason: "MCP resource subscriptions not yet implemented; deferred to follow-up".into(),
        })
    }
}
```

**Lifecycle in registry.** When a plugin manifest declares `mcp_servers {}` block (native Pattern plugin) — not the CC `.mcp.json` path — `PluginRegistry::install` constructs an `McpPluginAdapter` per server entry. Each becomes its own `LoadedPlugin` with a synthetic id like `mcp:<server>`.

CC plugins' `cc_mcp_servers` (Phase 4) are NOT routed through `McpPluginAdapter` — they go through Phase 5's runtime-side `McpRegistry` path (CC plugins inherit the runtime's MCP machinery). `McpPluginAdapter` is for *plugin-as-MCP-bridge* shape, where a Pattern-native plugin wants to package a single MCP server as its only contribution.

**Testing:**
Tests must verify each AC listed above:
- AC6.5: Install fixture Pattern-native plugin manifest declaring `mcp_servers { server "echo" { transport "stdio" command "..." } }`. Enable. Assert `port_registry.get("mcp:echo")` returns the port. Call the `echo` method via `port.call("echo", json!({"value":"x"}))`; assert response.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::mcp_adapter`.

**Commit:** `[pattern-runtime] add McpPluginAdapter wrapping standalone MCP server as PluginExtension`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 7-8) -->

<!-- START_TASK_7 -->
### Task 7: `pattern-plugin-sdk` smoke test + dep tree assertion

**Verifies:** AC6.8 final.

**Files:**
- Create: `crates/pattern_plugin_sdk/tests/smoke_minimal_plugin.rs`.
- Create: `crates/pattern_plugin_sdk/tests/fixtures/minimal_plugin/Cargo.toml` and `src/main.rs`.

**Implementation:**

A minimal plugin that compiles only against `pattern-plugin-sdk` with no extra features:

```rust
// tests/fixtures/minimal_plugin/src/main.rs
use pattern_plugin_sdk::{
    HookEvent, HookResponse, PluginContext, PluginError,
    PluginExtension, PortDeclaration, register_plugin, tags,
};

#[derive(Debug, Default)]
struct MinimalPlugin;

#[async_trait::async_trait]
impl PluginExtension for MinimalPlugin {
    fn ports(&self) -> Vec<PortDeclaration> { vec![] }

    async fn on_enable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        tracing::info!("minimal plugin enabled");
        Ok(())
    }

    fn on_event(&self, event: &HookEvent) -> Option<HookResponse> {
        if event.tag == tags::TURN_BEFORE {
            tracing::debug!(?event.tag, "minimal plugin saw turn.before");
        }
        None
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    register_plugin(MinimalPlugin::default()).await?;
    Ok(())
}
```

```toml
# tests/fixtures/minimal_plugin/Cargo.toml
[package]
name = "minimal_plugin"
version = "0.0.0"
edition = "2024"

[dependencies]
pattern-plugin-sdk = { path = "../../.." }
tokio = { version = "1.40", features = ["macros", "rt-multi-thread"] }
async-trait = "0.1"
tracing = "0.1"
tracing-subscriber = "0.3"
anyhow = "1"
```

The SDK smoke test:

```rust
#[test]
fn minimal_plugin_dep_tree_is_lean() {
    // 1. Build the fixture plugin.
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "--manifest-path", "tests/fixtures/minimal_plugin/Cargo.toml"])
        .status()
        .expect("cargo build minimal_plugin");
    assert!(status.success());

    // 2. Confirm dep tree omits forbidden crates.
    let output = std::process::Command::new(env!("CARGO"))
        .args(["tree", "--manifest-path", "tests/fixtures/minimal_plugin/Cargo.toml", "--no-default-features"])
        .output()
        .expect("cargo tree");
    let tree = String::from_utf8_lossy(&output.stdout);

    let forbidden = ["loro", "genai", "candle-core", "tokio-tungstenite", "rusqlite", "pattern-runtime", "pattern-memory"];
    for crate_name in &forbidden {
        assert!(
            !tree.contains(crate_name),
            "minimal plugin should NOT depend on {crate_name}; dep tree:\n{tree}"
        );
    }

    // 3. Confirm essential plugin types are reachable.
    // (Done at compile time of main.rs — if it builds, the imports resolved.)
}
```

**Testing:**
The smoke test IS the test.

**Verification:**
Run: `cargo nextest run -p pattern-plugin-sdk --test smoke_minimal_plugin`.

**Commit:** `[pattern-plugin-sdk] add minimal-plugin dep-tree smoke test`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Phase 6 integration suite — in-process + out-of-process + McpPluginAdapter

**Verifies:** AC6.1, AC6.2, AC6.3, AC6.4, AC6.5, AC6.6 (local), AC6.7, AC6.9, AC3.8 — end-to-end.

**Files:**
- Create: `crates/pattern_runtime/tests/plugin_transport.rs`.
- Create: `crates/pattern_runtime/tests/fixtures/plugins/oop-fixture/Cargo.toml`, `src/main.rs` — out-of-process fixture plugin.

**Implementation:**

```rust
#[tokio::test]
async fn out_of_process_plugin_full_cycle() {
    // 1. Build the OOP fixture binary as part of test setup.
    build_fixture_plugin("oop-fixture").expect("build OOP fixture");

    // 2. Spin up daemon with iroh Router on dual ALPNs.
    let env = TestEnv::with_dual_alpn().await;

    // 3. Install OOP fixture plugin — registry generates keypair, records pubkey.
    env.registry.install_local_path(
        Path::new("tests/fixtures/plugins/oop-fixture"),
        PluginScope::Global,
        &env.jj,
    ).await.unwrap();

    // 4. Enable; runtime spawns child + connects via QUIC.
    env.registry.enable("oop-fixture").await.unwrap();

    let plugin = env.registry.get("oop-fixture").unwrap();
    assert_eq!(plugin.connection.health(), PluginHealth::Healthy);

    // AC6.1: ports declared
    let ports = plugin.connection.declare_ports().await.unwrap();
    assert_eq!(ports.len(), 1);

    // AC6.2: port call round-trip
    let response = plugin.connection.port_call(
        ports[0].id.clone(),
        "echo",
        json!({"value":"hello"}),
    ).await.unwrap();
    assert_eq!(response, json!({"value":"hello"}));

    // AC6.3: plugin → runtime callbacks via PluginContext. Exercise THREE host-callback
    // paths in this single test (memory, send_message, task_create).
    env.memory_store.put_block("test/canary", "canary-content").await.unwrap();
    plugin.connection.on_install(&env.plugin_context()).await.unwrap();

    // (a) ctx.memory().get_block — fixture writes content to canary marker.
    let canary_marker = env.tempdir.path().join("oop_canary.txt");
    assert_eq!(std::fs::read_to_string(&canary_marker).unwrap(), "canary-content");

    // (b) ctx.send_message — fixture sent a message to a target agent during on_install.
    //     Assert mailbox observation.
    let mailbox = env.observe_mailbox(&AgentId::from("target-agent")).await;
    assert!(mailbox.iter().any(|m| m.body.contains("hello-from-plugin")));

    // (c) ctx.task_create — fixture created a task in a known TaskList block.
    let task_list = env.memory_store.get_block_metadata::<TaskListMetadata>("plugin/work").await.unwrap();
    assert!(task_list.items.iter().any(|t| t.subject == "scheduled by plugin"));

    // AC6.4: subscribe stream
    let mut stream = plugin.connection.port_subscribe(
        ports[0].id.clone(),
        json!({}),
    ).await.unwrap();
    use futures::StreamExt;
    let evt = stream.next().await.unwrap();
    assert!(matches!(evt, PortEvent::Line { .. }));

    // AC6.6 local auth: kill the fixture's pubkey from the allow-list,
    // restart, assert connection rejected.
    env.registry.tamper_remove_pubkey("oop-fixture");
    let result = env.registry.enable("oop-fixture").await;
    assert!(matches!(result, Err(PluginError::TransportLost { .. } | PluginError::InstallFailed { .. })));

    // AC6.7 / AC3.8: kill child mid-call.
    env.registry.tamper_restore_pubkey("oop-fixture");
    env.registry.enable("oop-fixture").await.unwrap();
    let plugin = env.registry.get("oop-fixture").unwrap();
    env.kill_plugin_process("oop-fixture");
    let err = plugin.connection.port_call(ports[0].id.clone(), "echo", json!({})).await.unwrap_err();
    assert!(matches!(err, PluginError::TransportLost { .. }));
    assert!(matches!(plugin.connection.health(), PluginHealth::Unhealthy { .. }));
}

#[tokio::test]
async fn in_process_plugin_zero_overhead() {
    // AC6.9: in-process CC adapter dispatch is direct trait call.
    let env = TestEnv::new().await;
    install_and_enable(&env, "cc-adapter-fixture").await;
    let plugin = env.registry.get("cc-adapter-fixture").unwrap();

    // Lightweight check: dispatch a port call; assert no IRPC encode/decode happens.
    // (Use a tracing subscriber to confirm no spans from the IRPC encode path fire.)
    let response = plugin.connection.port_call(/* ... */).await.unwrap();
    let _ = response;
}

#[tokio::test]
async fn mcp_plugin_adapter_wraps_standalone_server() {
    // AC6.5
    let env = TestEnv::new().await;
    let manifest_kdl = r#"
        name "echo-mcp-bridge"
        mcp_servers {
            server "echo" {
                transport "stdio"
                command "tests/fixtures/mcp/echo-server/server.sh"
            }
        }
    "#;
    let plugin_id = env.install_native_manifest_inline(manifest_kdl).await.unwrap();
    env.registry.enable(&plugin_id).await.unwrap();

    let port = env.port_registry.get(&"mcp:echo".into()).unwrap();
    let response = port.call("echo", json!({"value":"x"})).await.unwrap();
    assert_eq!(response, json!({"value":"x"}));
}
```

**Testing:** the integration tests above are the proof.

**Verification:**
Run: `cargo nextest run -p pattern-runtime --test plugin_transport`.

**Commit:** `[pattern-runtime] add Phase 6 plugin transport integration suite`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_D -->

---

## Phase done-when checklist

- [ ] `pattern_core` builds with `--no-default-features` and dep tree contains no `loro`/`genai`/`candle*`/`tokio-tungstenite`/`reqwest`/`rusqlite`.
- [ ] All consumer crates (pattern_memory, pattern_runtime, pattern_provider, pattern_db, pattern_server, pattern_cli) enable the right pattern-core features and build.
- [ ] `pattern-plugin-sdk` is a workspace member, builds with default features producing a slim dep graph.
- [ ] `PluginProtocol` IRPC service compiles; wire types round-trip via postcard.
- [ ] `PluginConnection` trait + `InProcessPluginConnection` (direct dispatch) + `OutOfProcessPluginConnection` (QUIC IRPC).
- [ ] Daemon migrated to `iroh::protocol::Router::builder().accept(b"pattern/1", ...).accept(b"pattern-plugin/1", ...).accept(b"pattern-plugin-memory-sync/1", ...).spawn()`.
- [ ] Iroh node-identity allow-list at install; rejects unknown peer keys at QUIC layer.
- [ ] Out-of-process plugin lifecycle: spawn child, supervise, surface `TransportLost`/`ProcessDied` on death.
- [ ] `McpPluginAdapter` wraps a single MCP server config as `PluginExtension`; tools accessible as port calls.
- [ ] All AC6 cases + AC3.8 pass under `cargo nextest run -p pattern-runtime --test plugin_transport`.
- [ ] `cargo nextest run --workspace` green.
- [ ] `cargo fmt` + `cargo clippy --all-features --all-targets` clean.

---

## Notes for executor

- **`pattern_core` feature audit is invasive.** Touches every consumer crate's Cargo.toml. Do it as one atomic commit so bisect doesn't land in a half-feature-gated state. Per project guidance: no shims, no commented-out `use` statements left behind.
- **Drop `chrono` entirely.** One usage; swap to `jiff::Timestamp::now()`. If grep finds others I missed, address each in this task.
- **`reqwest` and `tokio-tungstenite` are dropped from pattern_core's deps but stay elsewhere** (pattern-provider uses reqwest; HomeAssistant integration in pattern-runtime or its own crate uses tungstenite). Verify those crates have direct deps; do not rely on transitive resolution.
- **The `iroh::protocol::Router` migration in pattern_server** is a small refactor but is load-bearing. Do it FIRST (Task 5 step 1) so the rest of Task 5 can register the plugin protocol against a working router. Confirm TUI traffic still works after the migration via existing pattern_server integration tests before adding plugin protocol.
- **Plugin auth: pubkey allow-list, not encrypted handshake.** Phase 6 ships node-identity gating only — same machine, plugin user already trusted by the OS process boundary. Phase 7's atproto auth is for cross-machine. Don't conflate.
- **Out-of-process plugin process supervision is lifetime-coupled to the registry.** When `PluginRegistry::uninstall` removes the plugin, the supervisor task aborts and the child receives SIGTERM. Document the cleanup contract; don't leave zombie processes.
- **`McpPluginAdapter` and Phase 5's runtime `McpRegistry` are separate paths.** `McpRegistry` is for runtime-managed MCP servers (CC `.mcp.json` flow + persona/project KDL declarations). `McpPluginAdapter` is for plugins whose only contribution is a single bridged MCP server. They CAN coexist; agents calling `Pattern.Mcp.Call("foo", ...)` go through `McpRegistry`; agents calling `Pattern.Port.Call("mcp:foo", ...)` go through the plugin path.
- **Per project guidance:** if implicit work surfaces (e.g., feature-gating a module reveals broken `#[cfg]` paths in tests, or the `iroh::Router` migration breaks an existing test setup), fix them in scope. No stubs.
- **Reconnect-after-process-death is FUTURE WORK.** Phase 6 fails-and-surfaces when a child dies. Auto-restart with backoff is a follow-up plan; it's not in AC6 today and shouldn't accidentally land here.
- **`memory-sync` SDK feature is feature-defined and exercised at the protocol level (Task 3) but not yet by a fixture plugin.** Phase 6 adds `PluginMemorySync::{subscribe, apply_delta}` + `LoroDoc` re-export and registers the `pattern-plugin-memory-sync/1` ALPN; integration test (Task 8) opens a basic bidi sync session against a fixture runtime to prove the wire round-trips. First real plugin consumer can be a follow-up if needed.
- **Test fixtures live under `tests/fixtures/plugins/oop-fixture/`** as a separate Cargo package built by test setup. Not built by the workspace root by default — test setup invokes `cargo build --manifest-path ...` to keep fixture-build-cost out of the main workspace cycle.
