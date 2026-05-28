# v3-extensibility Phase 5: MCP inverted surface

**Goal:** Salvage the rmcp client from out-of-workspace `crates/pattern_mcp/src/client/` into `pattern_runtime/src/mcp/`. Replace the `Pattern.Mcp.Use` stub with `Call`/`Introspect`/`ListServers`/`Unload`. On MCP server load: inject a system-reminder pseudo-message into segment 2 listing servers + one-line-per-tool overview, and materialize each tool's full doc as a Working-tier Skill block at `mcp/<server>/<tool>.md` with `source: SkillSource::Mcp { server, tool }`. Per-server capability scoping. CC adapter's staged `cc_mcp_servers` from Phase 4 wires through.

**Architecture:** All MCP code paths live in `pattern_runtime` (NOT `pattern_core`); plugin SDK avoids rmcp transitive dep. Plugins that need to bridge their own local MCP servers expose those capabilities as Ports — they don't depend on Pattern's MCP client. Per-session `McpRegistry` manages live `rmcp::Service` connections, parallel to `FileManager`/`ProcessManager` ownership. Pre-existing client lifecycle lifted from `pattern_mcp/src/client/{service,transport,discovery}.rs`; the `tool_wrapper.rs` DynamicTool path is *not* salvaged — the inverted surface replaces it. Registry constructed at `open_with_agent_loop` from sources: persona KDL `mcp_servers {}` block, project `.pattern.kdl` `mcp_servers {}` block, plus the `cc_mcp_servers: Vec<McpServerConfig>` collected by Phase 4 from enabled CC plugins. On server connect: server's `list_tools` populates per-server tool metadata; `MessageAttachment::McpServerAvailable { server, tools_summary }` emitted into the next batch's segment 2 attachments; for each tool, a Working-tier Skill block is written at label `mcp/<server>/<tool>` with `source: SkillSource::Mcp { server, tool }` and the tool's parameter schema rendered into the body. Agents discover via FTS5 search (search hits include MCP tool docs), load via existing `Pattern.Skills.Load`, dispatch via `Pattern.Mcp.Call`. Capability scoping: `EffectCategory::Mcp` is the category; per-server check via `CapabilitySet::has_mcp_server(server)` modeled on `has_port(port_id)`. KDL config: `capabilities { mcp { servers "github" "filesystem" } }`. The `SkillSource` enum supersedes Phase 3's `source_plugin_id: Option<SmolStr>` — call sites get refactored in this phase per the v3-rewrite no-shims guidance.

**Tech Stack:** `rmcp` (workspace dep, already pinned via existing `pattern_mcp` crate's pin), `tokio` for async lifecycle, existing segment-2 attachment + FTS5 + Skill block infrastructure.

**Scope:** 5 of 7 phases.

**Codebase verified:** 2026-04-27.

---

## Codebase verification findings

- ✓ Salvage source at `crates/pattern_mcp/src/client/` (out-of-workspace): `service.rs:35-90` (`McpClientService` with `Option<ClientTransport>` + channel-based request loop), `transport.rs` (`ClientTransport` enum, stdio/http/sse), `discovery.rs` (`ToolDiscovery`). `tool_wrapper.rs` and `mod.rs` are NOT salvaged (inverted surface replaces the DynamicTool wrapping).
- ✓ `McpHandler` stub at `crates/pattern_runtime/src/sdk/handlers/mcp.rs:14-46` returns `not_implemented`. Wire shape: `McpReq::Use(String, String)`. Haskell mirror at `crates/pattern_runtime/haskell/Pattern/Mcp.hs:19-24` declares only `Use :: Server -> Method -> Mcp ()`.
- ✓ `MessageAttachment` enum at `crates/pattern_core/src/types/message.rs:119-300` with variants `BatchOpeningSnapshot`, `SkillAvailable`, `FileEdit`, `FileConflict`, `ShellOutput`, `PortEvent`, `BlockWriteNotifications`. Phase 5 adds `McpServerAvailable { server, tools_summary }`.
- ✓ Segment 2 attachment splice at `crates/pattern_provider/src/compose/passes/segment_2.rs:68-80`. `render_attachments_for_message()` wraps each in `<system-reminder>...</system-reminder>`. Adding the new variant + renderer follows the existing pattern.
- ✓ `BlockSchemaKind::Skill` (one of 7 variants); MCP tool docs reuse this schema with the new `source: SkillSource::Mcp { server, tool }` discriminator on `SkillMetadata`. FTS5 indexing covers Skill blocks by default. `Skills.Load` handler at `crates/pattern_runtime/src/sdk/handlers/skills.rs:97-180` already accepts any block-handle pointing at a Skill — works for MCP tool docs without modification.
- ✓ `EffectCategory::Mcp` exists in `CapabilitySet`. No `resources`-style per-id gating yet for MCP; mirror the `has_port(port_id)` pattern (category capability + per-id check at dispatch).
- ✓ `SessionContext::open_with_agent_loop` parameters list at `crates/pattern_runtime/CLAUDE.md::open_with_agent_loop` will gain `mcp_servers: Vec<McpServerConfig>` parameter (constructed by daemon from persona/project/CC sources before opening session).
- ⚠ Phase 3 introduced `source_plugin_id: Option<SmolStr>` on `SkillMetadata`. Phase 5 refactors to `source: Option<SkillSource>` enum. All Phase 3 + Phase 4 call sites updated in this phase per the v3-rewrite no-shims posture.
- ⚠ rmcp transport-streamable-http feature uses `reqwest`; already a workspace dep via `pattern-provider`. No new dep.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-extensibility.AC5: MCP inverted surface

- **v3-extensibility.AC5.1 Success:** On MCP server load, system reminder pseudo-message injected into segment 2 containing server name + one-line per tool
- **v3-extensibility.AC5.2 Success:** On MCP server load, Working-tier blocks created at `mcp/<server>/<tool>.md` with full tool documentation; searchable via `ctx.memory.search`
- **v3-extensibility.AC5.3 Success:** `ctx.mcp.call(server, method, args)` dispatches to the correct MCP server via rmcp; response returned to agent
- **v3-extensibility.AC5.4 Success:** `ctx.mcp.introspect(server)` returns structured tool metadata (name, description, input schema summary) for all tools on the server
- **v3-extensibility.AC5.5 Success:** `ctx.mcp.list_servers()` returns all loaded MCP servers with connection status
- **v3-extensibility.AC5.6 Success:** MCP server unload removes system reminder from subsequent turns and deletes tool doc blocks
- **v3-extensibility.AC5.7 Failure:** `ctx.mcp.call` to a server not in the agent's CapabilitySet returns `CapabilityError::Denied`
- **v3-extensibility.AC5.8 Failure:** `ctx.mcp.call` to a disconnected server returns `McpError::ServerUnavailable` with reconnection hint
- **v3-extensibility.AC5.9 Edge:** MCP server load/unload does not invalidate segment 1 cache (system prompt unchanged; only segment 2 system reminders change)
- **v3-extensibility.AC5.10 Edge:** MCP server stub deleted from codebase; `cargo check --workspace` passes without `pattern_mcp` in members list *(NOTE: Phase 7 deletes the `crates/pattern_mcp/` directory itself; Phase 5 ensures the workspace builds without depending on it.)*

---

## Tasks

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: Salvage rmcp client into `pattern_runtime::mcp`

**Verifies:** None directly (infrastructure foundation).

**Files:**
- Create: `crates/pattern_runtime/src/mcp.rs` (module root: re-exports + submodule declarations).
- Create: `crates/pattern_runtime/src/mcp/client.rs` (lifted from `pattern_mcp/src/client/service.rs`; renamed types, dropped DynamicTool wrapping).
- Create: `crates/pattern_runtime/src/mcp/transport.rs` (lifted from `pattern_mcp/src/client/transport.rs`).
- Create: `crates/pattern_runtime/src/mcp/error.rs` — `McpError` enum.
- Create: `crates/pattern_runtime/src/mcp/registry.rs` — `McpRegistry` (per-session manager).
- Modify: `crates/pattern_runtime/src/lib.rs` — `pub mod mcp;`.
- Modify: `crates/pattern_runtime/Cargo.toml` — add `rmcp = { workspace = true, features = ["client", "transport-child-process", "transport-streamable-http-client-reqwest", "client-side-sse"] }`.

**Implementation:**

Lift the rmcp wrapping. `McpClient` (renamed from `McpClientService`) owns one rmcp `Service` per server, bridges sync handler dispatch to async rmcp via channels:

```rust
use std::sync::Arc;
use parking_lot::RwLock;
use rmcp::Service;
use tokio::sync::{mpsc, oneshot};
use serde_json::Value;

use pattern_core::mcp::{McpServerConfig, McpTransport};

#[derive(Debug)]
pub struct McpClient {
    server_name: smol_str::SmolStr,
    state: parking_lot::Mutex<ClientState>,
}

#[derive(Debug)]
struct ClientState {
    request_tx: Option<mpsc::Sender<McpRequest>>,
    server_metadata: Option<ServerMetadata>,
    connected: bool,
}

#[derive(Debug)]
pub struct ServerMetadata {
    pub server_name: smol_str::SmolStr,
    pub server_description: Option<String>,
    pub tools: Vec<ToolMetadata>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolMetadata {
    pub name: smol_str::SmolStr,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

impl McpClient {
    pub async fn connect(config: &McpServerConfig) -> Result<Self, McpError> {
        // Match config.transport:
        //   Stdio { command, args, cwd } → rmcp transport-child-process
        //   Http  { url, headers }       → rmcp transport-streamable-http
        //   Sse   { url, headers }       → rmcp client-side-sse
        // Spawn the request-handler task and store request_tx.
        // Run rmcp's initialize handshake; cache server_metadata on success.
    }

    pub async fn call_tool(&self, method: &str, args: Value) -> Result<Value, McpError> { /* ... */ }

    pub fn server_metadata(&self) -> Option<ServerMetadata> { /* clone snapshot under lock */ }

    pub fn is_connected(&self) -> bool { /* read state */ }

    pub async fn disconnect(self) -> Result<(), McpError> {
        // Drop request_tx, await task join with grace timeout.
    }
}
```

`McpRegistry` is the per-session manager:

```rust
use dashmap::DashMap;

#[derive(Debug, Default)]
pub struct McpRegistry {
    clients: DashMap<smol_str::SmolStr, Arc<McpClient>>,
}

impl McpRegistry {
    pub async fn load_servers(&self, configs: &[McpServerConfig]) -> Vec<Result<smol_str::SmolStr, McpError>> {
        // For each config not already in self.clients, spawn a connection.
        // Returns Vec of (server_name, Result) for caller to log failures.
    }

    pub fn get(&self, server: &str) -> Option<Arc<McpClient>> { /* ... */ }
    pub fn list_connected(&self) -> Vec<smol_str::SmolStr> { /* ... */ }
    pub async fn unload(&self, server: &str) -> Result<(), McpError> { /* drop + disconnect */ }
}
```

`McpError`:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    #[error("MCP server {server} unavailable: {reason}; try `ctx.mcp.list_servers()` to verify connection state")]
    ServerUnavailable { server: smol_str::SmolStr, reason: String },

    #[error("MCP transport failed for {server}: {source}")]
    Transport { server: smol_str::SmolStr, #[source] source: rmcp::Error },

    #[error("MCP server {server} method {method} returned error: {message}")]
    ToolCallFailed { server: smol_str::SmolStr, method: String, message: String },

    #[error("MCP server {server} initialization failed: {message}")]
    InitFailed { server: smol_str::SmolStr, message: String },

    #[error("unknown MCP server: {server}")]
    UnknownServer { server: smol_str::SmolStr },
}
```

**Verification:**
Run: `cargo check -p pattern-runtime`.

**Commit:** `[pattern-runtime] salvage rmcp client into pattern_runtime::mcp`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Per-session `McpRegistry` lifecycle wiring

**Verifies:** None directly (infrastructure for AC5.3+).

**Files:**
- Modify: `crates/pattern_runtime/src/session.rs` — add `mcp_registry: Option<Arc<McpRegistry>>` to `SessionContext`; accept `mcp_servers: Vec<McpServerConfig>` parameter in `open_with_agent_loop`; load configs at session open.
- Modify: `crates/pattern_server/src/server.rs::get_or_open_session` — assemble `Vec<McpServerConfig>` from persona KDL + project KDL + enabled CC plugins' `cc_mcp_servers`; pass to `open_with_agent_loop`.
- Modify: `crates/pattern_runtime/src/persona_loader.rs` — parse new `mcp_servers {}` KDL block on personas, decoding to `Vec<McpServerConfig>`. Mirror parsing for project `.pattern.kdl`.

**Implementation:**

`SessionContext` owns the registry. `open_with_agent_loop` constructs it and calls `load_servers` after the session machinery is initialized but before the eval worker spawns:

```rust
let mcp_registry = Arc::new(McpRegistry::default());
let load_results = mcp_registry.load_servers(&mcp_servers).await;
for (server, result) in load_results {
    if let Err(e) = result {
        tracing::warn!(?server, ?e, "failed to load MCP server at session open");
    }
}
ctx = ctx.with_mcp_registry(mcp_registry);
```

KDL parsing on personas:

```kdl
mcp_servers {
    server "github" {
        transport "stdio"
        command "npx"
        args "@modelcontextprotocol/server-github"
        env "GITHUB_TOKEN" "keychain:github-token"
    }
    server "internal-api" {
        transport "http"
        url "https://api.example.com/mcp"
        headers "Authorization" "Bearer keychain:api-token"
    }
}
```

Decoded via knus DTOs into `Vec<McpServerConfig>`. Same shape works in `.pattern.kdl`.

**Testing:**
- Open session with two-server config (one stdio + one http); assert both connect; `registry.list_connected()` returns both names.
- Persona with no mcp_servers block + project with `mcp_servers {}` block: registry loads project servers.
- Connection failure on one server: warn-level log fires; session opens; `registry.list_connected()` excludes the failed server.

**Verification:**
Run: `cargo nextest run -p pattern-runtime mcp::registry`.

**Commit:** `[pattern-runtime] wire per-session McpRegistry + persona/project KDL parsing`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->

<!-- START_TASK_3 -->
### Task 3: New `Pattern.Mcp` wire shape — `Call` / `Introspect` / `ListServers` / `Unload`

**Verifies:** Foundation for AC5.3, AC5.4, AC5.5.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/requests/mcp.rs` — replace `McpReq::Use(String, String)` with the new variants.
- Modify: `crates/pattern_runtime/haskell/Pattern/Mcp.hs` — replace GADT.
- Modify: `crates/pattern_runtime/src/sdk/handlers/mcp.rs` — `effect_decl()` describes new constructors.

**Implementation:**

Wire types in Rust:

```rust
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

#[derive(Debug, Clone, Serialize, Deserialize, tidepool_repr::FromCore)]
#[non_exhaustive]
pub enum McpReq {
    #[core(module = "Pattern.Mcp", name = "Call")]
    Call { server: SmolStr, method: SmolStr, args: serde_json::Value },

    #[core(module = "Pattern.Mcp", name = "Introspect")]
    Introspect { server: SmolStr },

    #[core(module = "Pattern.Mcp", name = "ListServers")]
    ListServers,

    #[core(module = "Pattern.Mcp", name = "Unload")]
    Unload { server: SmolStr },
}

#[derive(Debug, Clone, Serialize, Deserialize, tidepool_repr::ToCore)]
pub struct McpServerSummary {
    pub server: SmolStr,
    pub connected: bool,
    pub tool_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, tidepool_repr::ToCore)]
pub struct McpIntrospection {
    pub server: SmolStr,
    pub description: Option<String>,
    pub tools: Vec<ToolMetadata>,
}
```

Haskell GADT:

```haskell
{-# LANGUAGE GADTs, NoImplicitPrelude #-}
module Pattern.Mcp where

import Pattern.Prelude
import qualified Pattern.Aeson as Aeson

type Server = Text
type Method = Text

data Mcp a where
  Call        :: Server -> Method -> Aeson.Value -> Mcp Aeson.Value
  Introspect  :: Server -> Mcp McpIntrospection
  ListServers :: Mcp [McpServerSummary]
  Unload      :: Server -> Mcp ()

data McpServerSummary = McpServerSummary
  { mssServer     :: Text
  , mssConnected  :: Bool
  , mssToolCount  :: Int
  }

data McpIntrospection = McpIntrospection
  { miServer       :: Text
  , miDescription  :: Maybe Text
  , miTools        :: [ToolMetadata]
  }

-- Helpers
call :: Member Mcp effs => Server -> Method -> Aeson.Value -> Eff effs Aeson.Value
call s m args = Freer.send (Call s m args)

introspect :: Member Mcp effs => Server -> Eff effs McpIntrospection
introspect s = Freer.send (Introspect s)

listServers :: Member Mcp effs => Eff effs [McpServerSummary]
listServers = Freer.send ListServers

unload :: Member Mcp effs => Server -> Eff effs ()
unload s = Freer.send (Unload s)
```

`effect_decl()` updated to describe the four new constructors with proper helpers/examples.

**Testing:**
- Round-trip: `McpReq::Call { ... }` → tidepool wire → back. Assert decode equality.
- Each variant deserializes from a Haskell-emitted Core value with the right datacon name.

**Verification:**
Run: `cargo nextest run -p pattern-runtime mcp::wire`.

**Commit:** `[pattern-runtime] replace Pattern.Mcp wire shape — Call/Introspect/ListServers/Unload`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Replace `McpHandler` stub with real dispatch

**Verifies:** AC5.3, AC5.4, AC5.5, AC5.6, AC5.8.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/mcp.rs` — full impl.

**Implementation:**

```rust
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::session::{HasCancelState, HasMcpRegistry, SessionContext};
use crate::sdk::requests::McpReq;

#[derive(Default, Clone, Debug)]
pub struct McpHandler;

impl EffectHandler<SessionContext> for McpHandler {
    type Request = McpReq;

    fn handle(&mut self, req: McpReq, cx: &EffectContext<'_, SessionContext>) -> Result<Value, EffectError> {
        let _guard = HandlerGuard::enter(&cx.user().cancel_state().gate);
        let registry = cx.user().mcp_registry().ok_or_else(|| EffectError::Handler(
            "MCP registry not initialized for this session".into()
        ))?;

        let runtime_handle = cx.user().tokio_handle();
        match req {
            McpReq::Call { server, method, args } => {
                // Capability gate: cx.user().capabilities().has_mcp_server(&server) — Task 7.
                if !cx.user().capabilities().map(|c| c.has_mcp_server(&server)).unwrap_or(true) {
                    return Err(EffectError::Handler(format!(
                        "{}MCP server not in capability set: {server}",
                        crate::policy::PERMISSION_DENIED_PREFIX,
                    )));
                }

                let client = registry.get(&server).ok_or_else(|| EffectError::Handler(
                    format!("unknown MCP server: {server}; available: {:?}", registry.list_connected())
                ))?;

                // Sync→async bridge via tokio_handle.block_on (bounded; no plugin code in await).
                let response = runtime_handle.block_on(client.call_tool(&method, args))
                    .map_err(|e| match e {
                        crate::mcp::McpError::ServerUnavailable { reason, .. } =>
                            EffectError::Handler(format!("MCP server {server} unavailable: {reason}; reconnect via session restart")),
                        crate::mcp::McpError::ToolCallFailed { message, .. } =>
                            EffectError::Handler(format!("MCP {server}.{method} failed: {message}")),
                        other => EffectError::Handler(other.to_string()),
                    })?;
                Ok(json_to_core_value(response))
            }
            McpReq::Introspect { server } => {
                let client = registry.get(&server).ok_or_else(|| EffectError::Handler(
                    format!("unknown MCP server: {server}")
                ))?;
                let metadata = client.server_metadata().ok_or_else(|| EffectError::Handler(
                    format!("MCP server {server} not connected")
                ))?;
                Ok(introspection_to_core_value(metadata))
            }
            McpReq::ListServers => {
                let summaries: Vec<_> = registry.list_connected().into_iter().map(|server| {
                    let client = registry.get(&server).expect("listed");
                    let tool_count = client.server_metadata().map(|m| m.tools.len() as u32).unwrap_or(0);
                    McpServerSummary { server, connected: client.is_connected(), tool_count }
                }).collect();
                Ok(server_summary_list_to_core_value(summaries))
            }
            McpReq::Unload { server } => {
                runtime_handle.block_on(registry.unload(&server))
                    .map_err(|e| EffectError::Handler(e.to_string()))?;
                // Tear down the McpServerAvailable attachment + Skill blocks (Task 5+6 hooks).
                fire_unload_side_effects(cx, &server);
                Ok(Value::unit())
            }
        }
    }
}
```

`block_on` is safe per the established policy: bounded await target, no plugin code in the await path, blocking matches the semantic contract. Same shape as the spawn handler's pattern.

`fire_unload_side_effects` calls into the attachment + skill block teardown helpers from Tasks 5 and 6.

`HasMcpRegistry` is a new trait module mirroring `HasFileManager` / `HasProcessManager`:

```rust
pub trait HasMcpRegistry { fn mcp_registry(&self) -> Option<&Arc<McpRegistry>>; }
impl HasMcpRegistry for SessionContext { /* ... */ }
impl HasMcpRegistry for () { fn mcp_registry(&self) -> Option<&Arc<McpRegistry>> { None } }
```

**Testing:**
Tests must verify each AC listed above:
- AC5.3 Call: dispatch `McpReq::Call` to a fixture stdio server (a tiny script speaking the MCP protocol via stdio); assert the response value matches expected.
- AC5.4 Introspect: assert the returned `McpIntrospection` includes all tools the fixture server exposes.
- AC5.5 ListServers: register two servers; assert both appear in `ListServers` response with correct connection state.
- AC5.6 Unload: call Unload; assert subsequent `ListServers` excludes; assert Skill blocks for that server's tools are gone (verified in Task 6).
- AC5.8 Server unavailable: kill the fixture server's process; dispatch Call; assert `EffectError::Handler` mentions "unavailable" and the "reconnect" hint.

**Verification:**
Run: `cargo nextest run -p pattern-runtime mcp::handler`.

**Commit:** `[pattern-runtime] replace Mcp handler stub with real dispatch via McpRegistry`
<!-- END_TASK_4 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 5-6) -->

<!-- START_TASK_5 -->
### Task 5: `MessageAttachment::McpServerAvailable` + segment 2 render

**Verifies:** AC5.1, AC5.6, AC5.9.

**Files:**
- Modify: `crates/pattern_core/src/types/message.rs:119-300` — add `McpServerAvailable { server, tools_summary }` variant.
- Modify: `crates/pattern_provider/src/compose/render.rs:44-94` — render the new variant inside `<system-reminder>`.
- Modify: `crates/pattern_runtime/src/mcp/registry.rs` — emit `McpServerAvailable` attachments to `SessionContext.async_reminder_queue` on server load; push removal on unload.

**Implementation:**

New variant:

```rust
// in pattern_core::types::message::MessageAttachment
McpServerAvailable {
    server: smol_str::SmolStr,
    description: Option<String>,
    tools_summary: Vec<McpToolSummaryLine>, // server_name, one-line per tool
},
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolSummaryLine {
    pub tool_name: smol_str::SmolStr,
    pub one_line: String,  // e.g., "create_issue: file a new issue against a repo"
}
```

Renderer:

```rust
// in render.rs
fn render_mcp_server_available(server: &str, description: &Option<String>, tools: &[McpToolSummaryLine]) -> String {
    let mut s = String::new();
    s.push_str(&format!("<system-reminder>\n[mcp:server-available] {server}"));
    if let Some(desc) = description {
        s.push_str(&format!(" — {desc}"));
    }
    s.push('\n');
    for t in tools {
        s.push_str(&format!("  - {}: {}\n", t.tool_name, t.one_line));
    }
    s.push_str("Load tool details via Skills.Load(\"mcp/<server>/<tool>\"). Dispatch via Pattern.Mcp.Call(server, method, args).\n");
    s.push_str("</system-reminder>\n");
    s
}
```

Emission: in `McpRegistry::load_servers`, for each successful connection, push `MessageAttachment::McpServerAvailable { ... }` to `cx.user().async_reminder_queue` (the existing queue used by file/port subsystems for delivery at next-turn boundary). On `Unload`, push a paired `MessageAttachment::McpServerUnavailable { server }` (also a new variant; renders as a one-line system reminder noting removal). Phase 5 ships both halves.

Cache stability (AC5.9): segment 1 is the system prompt — never touched by these attachments. Segment 2 attachments are ephemeral per turn; adding/removing reminders only invalidates segment 2 cache, not segment 1.

**Testing:**
Tests must verify each AC listed above:
- AC5.1: Load fixture MCP server with two tools; observe the next batch's segment 2 contains a `<system-reminder>` block listing both tools.
- AC5.6: Unload the server; assert the next batch's segment 2 does NOT contain the `McpServerAvailable` reminder.
- AC5.9: Load + unload cycle; assert segment 1 of the composed request is byte-identical before and after (use `assert_eq!` on the segment 1 string).

**Verification:**
Run: `cargo nextest run -p pattern-runtime mcp::reminder` and `cargo nextest run -p pattern-provider compose::passes::segment_2`.

**Commit:** `[pattern-core] [pattern-provider] [pattern-runtime] add McpServerAvailable attachment + segment 2 render + load/unload emission`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: MCP tool docs as Skill blocks (refactor `SkillSource` enum)

**Verifies:** AC5.2, AC5.6.

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/skill.rs` — replace `source_plugin_id: Option<SmolStr>` with `source: Option<SkillSource>` enum.
- Modify: `crates/pattern_runtime/src/plugin/cc_adapter/skills.rs` — update CC skill translator to emit `Some(SkillSource::Plugin { plugin_id })`.
- Modify: `crates/pattern_runtime/src/sdk/handlers/skills.rs:171` — update Memory.Put-driven path to `source: None`.
- Create: `crates/pattern_runtime/src/mcp/tool_docs.rs` — materialize tool docs as Skill blocks on server load; tear down on unload.

**Implementation:**

`SkillSource` enum:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SkillSource {
    Plugin { plugin_id: smol_str::SmolStr },
    Mcp { server: smol_str::SmolStr, tool: smol_str::SmolStr },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SkillMetadata {
    pub name: String,
    pub trust_tier: SkillTrustTier,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub hooks: serde_json::Value,
    /// REPLACES source_plugin_id from Phase 3. Captures plugin OR MCP origin.
    /// `#[serde(default)]` so existing on-disk skill blocks deserialize unchanged.
    #[serde(default)]
    pub source: Option<SkillSource>,
}
```

Tool docs materialization in `mcp/tool_docs.rs`:

```rust
use std::sync::Arc;

use pattern_core::types::memory_types::skill::{SkillMetadata, SkillSource, SkillTrustTier};
use pattern_memory::block::Block;
use pattern_memory::MemoryStore;

use crate::mcp::ToolMetadata;

pub async fn materialize_tool_docs(
    store: &Arc<dyn MemoryStore>,
    server: &str,
    tools: &[ToolMetadata],
) -> Result<Vec<smol_str::SmolStr>, McpError> {
    let mut block_handles = Vec::new();
    for tool in tools {
        let label = format!("mcp/{server}/{}", tool.name);
        let body = render_tool_doc_body(server, tool);
        let metadata = SkillMetadata {
            name: tool.name.to_string(),
            trust_tier: SkillTrustTier::PluginInstalled, // MCP tools come from external sources; tier matches plugin-installed
            description: tool.description.clone(),
            keywords: extract_keywords_from_schema(&tool.input_schema),
            hooks: serde_json::Value::Null,
            source: Some(SkillSource::Mcp { server: server.into(), tool: tool.name.clone() }),
        };
        // Persist via existing Skill block creation path; Working tier.
        store.create_or_update_skill_block(&label, metadata, body).await?;
        block_handles.push(label.into());
    }
    Ok(block_handles)
}

pub async fn delete_tool_docs(
    store: &Arc<dyn MemoryStore>,
    server: &str,
) -> Result<(), McpError> {
    let prefix = format!("mcp/{server}/");
    store.delete_blocks_with_prefix(&prefix).await?;
    Ok(())
}

fn render_tool_doc_body(server: &str, tool: &ToolMetadata) -> String {
    // Markdown body:
    //   # {tool.name}
    //   {tool.description}
    //   ## Server: {server}
    //   ## Input schema
    //   ```json
    //   {input_schema pretty-printed}
    //   ```
    //   ## Usage
    //   Pattern.Mcp.Call(server, method, args) — args matches the schema above.
}
```

Wired into `McpRegistry::load_servers`: after each connection succeeds and metadata is fetched, call `materialize_tool_docs(store, &server, &metadata.tools)`. On `unload`, call `delete_tool_docs`.

**Testing:**
Tests must verify each AC listed above:
- AC5.2: Load fixture MCP server with two tools; assert two Skill blocks exist at `mcp/<server>/<tool>` labels; assert each carries `source: SkillSource::Mcp { server, tool }` and `trust_tier: PluginInstalled`. FTS5: search for one of the tool names; assert the corresponding block is in the result set. `Skills.Load(&handle)` on one of these blocks returns the rendered body.
- AC5.6: Unload the server; assert all `mcp/<server>/*` Skill blocks are deleted.
- Refactor regression: existing CC adapter Skill creation produces blocks with `source: Some(SkillSource::Plugin { plugin_id })`; existing project-local skills have `source: None`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime mcp::tool_docs` and `cargo nextest run -p pattern-runtime plugin::cc_adapter::skills`.

**Commit:** `[pattern-core] [pattern-runtime] refactor SkillSource enum + materialize MCP tool docs as Skill blocks`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 7-8) -->

<!-- START_TASK_7 -->
### Task 7: Per-server capability scoping

**Verifies:** AC5.7.

**Files:**
- Modify: `crates/pattern_core/src/capability.rs:178-202` — `CapabilitySet.resources` for `EffectCategory::Mcp` carries the allow-listed server names; new `has_mcp_server(server)` accessor.
- Modify: `crates/pattern_runtime/src/persona_loader.rs` — parse new `mcp { servers ... }` block under `capabilities {}`.
- Modify: `crates/pattern_runtime/src/sdk/handlers/mcp.rs` — gate `Call` and `Introspect` on `has_mcp_server`.

**Implementation:**

Mirror `has_port(port_id)` shape. `CapabilitySet.resources` is `BTreeMap<EffectCategory, BTreeSet<SmolStr>>`. For MCP:

```rust
impl CapabilitySet {
    /// Returns true if the agent's capability set includes Mcp category AND
    /// either has no resource restriction (full access) OR explicitly allow-lists this server.
    pub fn has_mcp_server(&self, server: &str) -> bool {
        if !self.categories.contains(&EffectCategory::Mcp) {
            return false;
        }
        match self.resources.get(&EffectCategory::Mcp) {
            None => true,                  // No restriction — full Mcp access.
            Some(allowed) if allowed.is_empty() => true,
            Some(allowed) => allowed.contains(server),
        }
    }
}
```

KDL parsing for the persona block:

```kdl
capabilities {
    effects { mcp; memory; message }
    resources {
        mcp {
            servers "github" "filesystem"
        }
    }
}
```

Resources block decodes to `BTreeMap<EffectCategory, BTreeSet<SmolStr>>`. The persona-loader DTO grows accordingly.

The Call handler gate:

```rust
if !cx.user().capabilities().map(|c| c.has_mcp_server(&server)).unwrap_or(true) {
    return Err(EffectError::Handler(format!(
        "{}MCP server '{}' not in capability set",
        crate::policy::PERMISSION_DENIED_PREFIX,
        server,
    )));
}
```

Same gate applied in `Introspect`. `ListServers` is unrestricted (it's a metadata-only operation; agents always know which servers exist if they're in the session).

**Testing:**
Tests must verify each AC listed above:
- AC5.7: Construct session with `CapabilitySet { categories: {Mcp}, resources: {Mcp -> {"allowed-server"}} }`; load two MCP servers ("allowed-server", "denied-server"); dispatch Call to denied-server; assert `EffectError::Handler` with `PERMISSION_DENIED_PREFIX`.
- Edge: `CapabilitySet` with `Mcp` category but no `resources[Mcp]` entry → `has_mcp_server("anything")` returns true (full access).
- Edge: `CapabilitySet` without `Mcp` category at all → `has_mcp_server` returns false; dispatch returns capability-denied.

**Verification:**
Run: `cargo nextest run -p pattern-core capability::has_mcp_server` and `cargo nextest run -p pattern-runtime mcp::handler`.

**Commit:** `[pattern-core] [pattern-runtime] add per-server MCP capability scoping`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: AC5 integration suite

**Verifies:** AC5.1 through AC5.10 — end-to-end.

**Files:**
- Create: `crates/pattern_runtime/tests/mcp_inverted.rs`.
- Create: `crates/pattern_runtime/tests/fixtures/mcp/echo-server/` — tiny stdio MCP server fixture (Python or Bash script implementing the MCP handshake + a couple of tools).

**Implementation:**

```rust
#[tokio::test]
async fn mcp_inverted_surface_full_cycle() {
    let env = TestEnv::new().await;

    // Configure session with one fixture stdio server.
    let configs = vec![McpServerConfig {
        name: "echo".into(),
        transport: McpTransport::Stdio {
            command: "tests/fixtures/mcp/echo-server/server.sh".into(),
            args: vec![], cwd: None,
        },
        env: Default::default(),
        disabled: false,
    }];
    let session = env.open_session_with_mcp(configs).await;

    // AC5.1: system reminder injected on next turn.
    let composed = env.compose_next_turn(&session).await;
    assert!(composed.segment2.contains("[mcp:server-available] echo"));
    assert!(composed.segment2.contains("- echo: returns the input"));

    // AC5.2: tool docs as Skill blocks.
    let block = env.memory_store.get_block("mcp/echo/echo").await.unwrap();
    assert!(matches!(block.metadata.source, Some(SkillSource::Mcp { .. })));
    assert_eq!(block.metadata.trust_tier, SkillTrustTier::PluginInstalled);

    // FTS5 search hit.
    let hits = env.memory_store.search("input", BlockSchema::Skill).await.unwrap();
    assert!(hits.iter().any(|h| h.label == "mcp/echo/echo"));

    // AC5.3: Call dispatches.
    let response = handlers::mcp::handle_call(&session.ctx, "echo", "echo", json!({"value":"hi"})).await.unwrap();
    assert_eq!(response, json!({"value":"hi"}));

    // AC5.4: Introspect.
    let intro = handlers::mcp::handle_introspect(&session.ctx, "echo").await.unwrap();
    assert_eq!(intro.tools.len(), 1);

    // AC5.5: ListServers.
    let list = handlers::mcp::handle_list_servers(&session.ctx).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].connected);

    // AC5.7: capability denial.
    let denied_session = env.open_session_with_mcp_and_caps(configs.clone(), CapabilitySet {
        categories: btreeset!{EffectCategory::Mcp},
        resources: btreemap!{EffectCategory::Mcp => btreeset!{}},
        flags: Default::default(),
    }).await;
    let err = handlers::mcp::handle_call(&denied_session.ctx, "echo", "echo", json!({})).await.unwrap_err();
    assert!(err.to_string().contains(PERMISSION_DENIED_PREFIX));

    // AC5.8: server unavailable.
    env.kill_fixture_server("echo");
    let err = handlers::mcp::handle_call(&session.ctx, "echo", "echo", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("unavailable"));

    // AC5.6: Unload tears down.
    handlers::mcp::handle_unload(&session.ctx, "echo").await.unwrap();
    let composed = env.compose_next_turn(&session).await;
    assert!(!composed.segment2.contains("[mcp:server-available] echo"));
    let block = env.memory_store.get_block("mcp/echo/echo").await;
    assert!(block.is_err() || block.unwrap().is_none());

    // AC5.9: segment 1 cache stability.
    let s1_before = composed.segment1.clone();
    handlers::mcp::handle_load(&session.ctx, &configs).await.unwrap();
    let composed_after = env.compose_next_turn(&session).await;
    assert_eq!(s1_before, composed_after.segment1, "segment 1 must be unchanged across MCP load/unload");

    // AC5.10: workspace builds without pattern_mcp.
    // (verified by cargo check --workspace passing in CI; not a runtime test)
}
```

The fixture stdio server is a simple Python or Bash script that implements just enough MCP protocol to respond to `initialize`, `list_tools`, and `tools/call` for one `echo` tool. Lives at `tests/fixtures/mcp/echo-server/server.sh` (or `.py` if cleaner — Python's `mcp` library is the official SDK; Bash is theoretically possible but more error-prone).

**Testing:** the integration test above is the proof.

**Verification:**
Run: `cargo nextest run -p pattern-runtime --test mcp_inverted`.

**Commit:** `[pattern-runtime] add MCP inverted surface integration suite`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_D -->

---

## Phase done-when checklist

- [ ] `pattern_runtime::mcp` module compiles with `client.rs`, `transport.rs`, `registry.rs`, `tool_docs.rs`, `error.rs`.
- [ ] `pattern_runtime` depends on `rmcp` directly; `pattern_core` does NOT pull rmcp.
- [ ] `Pattern.Mcp` GADT replaces `Use` with `Call`/`Introspect`/`ListServers`/`Unload`; Haskell helpers ship.
- [ ] `McpHandler` dispatches all four ops; capability gate on Call + Introspect.
- [ ] `MessageAttachment::McpServerAvailable` + `McpServerUnavailable` variants render in segment 2 inside `<system-reminder>`.
- [ ] On server load: tool docs materialize as Working-tier Skill blocks at `mcp/<server>/<tool>` with `source: Some(SkillSource::Mcp { server, tool })`. FTS5 search returns them.
- [ ] On server unload: attachment removed from queue, Skill blocks deleted.
- [ ] `SkillMetadata::source: Option<SkillSource>` replaces Phase 3's `source_plugin_id`; CC adapter call sites updated to emit `SkillSource::Plugin { plugin_id }`.
- [ ] `CapabilitySet::has_mcp_server` works; persona KDL `capabilities { resources { mcp { servers ... } } }` parses.
- [ ] Workspace builds without depending on `crates/pattern_mcp/`. (Phase 7 deletes the directory.)
- [ ] All AC5 cases pass under `cargo nextest run -p pattern-runtime --test mcp_inverted`.
- [ ] `cargo nextest run --workspace` green.
- [ ] `cargo fmt` + `cargo clippy --all-features --all-targets` clean.

---

## Notes for executor

- **Plugins do NOT depend on the MCP client.** Plugins that want to bridge their own local MCP servers (e.g., remote IRPC plugins with stdio MCP on their machine) expose those capabilities as Pattern Ports, not by linking the MCP client. Document this in `pattern_runtime/CLAUDE.md` after Phase 5 lands so the boundary is explicit.
- **`SkillSource` refactor is breaking by design.** Phase 3's `source_plugin_id` field is removed in this phase, not deprecated. Update every call site in the same commit. Per project guidance: no backwards-compat shims during v3 rewrite.
- **`block_on` in the handler is policy-compliant.** The await target is bounded (rmcp call_tool inherits the per-server timeout), no plugin code in the await path. Same shape as the spawn handler. If lifecycle work later forces a different shape (e.g., Mcp.Subscribe for resource subscriptions, which would be long-lived), revisit then.
- **rmcp `transport-streamable-http` requires `reqwest`.** Already a workspace dep via pattern-provider — no new dep. If the rmcp version pin requires a newer `reqwest` than the workspace ships, surface that as a dep-bump question rather than papering over.
- **The fixture echo server should live in this repo, not be downloaded.** Python `mcp` library + a 30-line `server.py` is the cleanest. If the test environment doesn't have Python's `mcp` installed, the test gates with `#[ignore]` and an explanation comment — but the executor should ensure the test can run unattended in CI by adding the dep through dev-dependencies or a `tests/fixtures/mcp/echo-server/requirements.txt`.
- **Per-server capability scoping uses `resources` not a new field.** This is a broader pattern Pattern's capability system can adopt for other categories later (Port already does it via `has_port`). Keeping consistent shape avoids per-category bespoke types.
- **Phase 7 deletes `crates/pattern_mcp/`.** Do not delete it in Phase 5 — keep the salvage source live until the inverted surface is fully working. Phase 7's audit task verifies the workspace is clean and removes the directory.
- **Future-considerations not in scope here:**
  - Reconnection strategy on transient server crash (currently: surface Unavailable error; agent / human triggers reload).
  - Server-side resource subscriptions (mcp `resources/subscribe`) — would be a `Pattern.Mcp.Subscribe` adding to the wire shape later.
  - Streaming responses — current shape is unary call. If MCP servers ship streaming methods that Pattern wants to consume, add `Pattern.Mcp.CallStream` later.
- **Per project guidance:** if implicit work surfaces (e.g., `MemoryStore::create_or_update_skill_block` doesn't yet exist as a single method), add it now; don't spread the operation across read+delete+create chains. Cleanly extending storage APIs is in scope.
