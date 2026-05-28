# v3-extensibility Phase 4: CC adapter completion + Haskell compatibility library

**Goal:** Complete the CC adapter so installed CC plugins fully exercise their declared surface. Translate `agents/<name>.md` to in-memory `EphemeralConfig` spawn targets. Translate `monitors/<name>.json` to `Port` implementations streaming stdout as `PortEvent`s. Build a slash-command registry + dispatch layer in `pattern_server` (currently a placeholder) and translate `commands/<name>.md` into registered slash commands with audience-tier from each command's frontmatter. Inject CC `bin/` into the session shell PATH. Normalize CC `.mcp.json` entries into a `pattern_core::mcp::McpServerConfig` collection on `LoadedPlugin` for Phase 5 consumption. Ship a `Pattern.Cc` Haskell compatibility module spliced into the agent prelude when at least one CC plugin is enabled.

**Architecture:** Translations split between one-shot and per-session work. At `CcPluginAdapter::on_install`: parse all artifacts (agents, monitors, commands, mcp configs) and stage them on the `LoadedPlugin`. At `on_enable`: spawn monitor processes, register slash commands, augment session PATH, splice `Pattern.Cc` into the prelude tempdir if not yet present, hand staged MCP configs to Phase 5's loader. Spawn configs constructed in-memory only — `pattern.persona_mode = "draft"` on the manifest opts an agent into draft KDL writing for later promotion, but the default is purely in-memory. Monitors are per-session `Port` impls registered into `PortRegistryImpl`; the existing dispatcher handles drain. Slash commands lift from a single placeholder RPC to a `CommandRegistry` (in `pattern_server`) with `command_name → Box<dyn CommandHandler>` mappings, audience-tier per command, structured `CommandResponse { content, kind, side_effects }` return type. `RunCommand` RPC swapped from string-return to `CommandResponse`-return. CC `.mcp.json` entries normalized into `McpServerConfig` instances stored on `LoadedPlugin`; Phase 5 consumes from there. `Pattern.Cc` is a `&'static str` module shipped with the runtime, materialized to per-session prelude tempdir via the existing `port_libraries` plumbing — only included when CC plugins are present.

**Tech Stack:** Existing surface — `EphemeralConfig`, `Port` trait, `PortRegistryImpl`, `ProcessManager`, prelude `build_with_libraries`, `Author` origin. New: `CommandRegistry` infrastructure in `pattern_server`. New types: `pattern_core::mcp::McpServerConfig`, `pattern_core::commands::CommandResponse`. No new external deps.

**Scope:** 4 of 7 phases.

**Codebase verified:** 2026-04-27.

---

## Codebase verification findings

- ✓ `EphemeralConfig::new(program)` at `crates/pattern_core/src/spawn.rs:32-47` constructs in-memory; builders for costume, capabilities, timeout. Direct fit for CC agent frontmatter.
- ✓ `Port` trait at `crates/pattern_core/src/traits/port.rs:84-180` returns `BoxStream<PortEvent>` from `subscribe()`. Dispatcher actor (`crates/pattern_runtime/src/port_registry/dispatcher.rs`) handles drain.
- ⚠ **Slash command infrastructure is a placeholder.** `pattern_server::protocol::PatternMessage::RunCommand` at `protocol.rs:353` exists; handler at `server.rs:958-969` returns `"plugin command not yet implemented"`. Built-in commands (`/agent`, `/promote`, `/relate`) route through dedicated RPCs — not `RunCommand`. Phase 4 builds the dispatch + registry from scratch and switches the wire return from `String` to `CommandResponse`.
- ✓ `LocalPtyBackend::with_env(env)` at `process_manager/local_pty.rs:174-216` accepts per-session env. `ProcessManager` is owned by `SessionContext`, so PATH augmentation per CC plugin is contained.
- ⚠ Out-of-workspace `pattern_mcp` crate has CC-shape config at `pattern_mcp/src/client/service.rs::McpServerConfig`. Phase 4's normalized type lives in `pattern_core::mcp` and is structurally equivalent; Phase 5's MCP loader consumes that. No coupling to the salvage timeline.
- ✓ `build_with_libraries(decls, port_libraries)` at `crates/pattern_runtime/src/sdk/preamble.rs:87-180` splices Haskell sources into the prelude. Conditional inclusion is a one-line gate at session-open.
- ✓ `Author` enum at `crates/pattern_core/src/types/origin.rs:193-203` distinguishes Partner/Agent. `bypasses_permission_gate()` returns `true` for Partner. Audience-tier checks reuse this surface.
- ✓ `RuntimeConfigWriter` (`crates/pattern_runtime/src/spawn/draft.rs`) is available for opt-in draft KDL writing when `pattern.persona_mode = "draft"`.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-extensibility.AC3: CC plugin adapter (remaining cases)

- **v3-extensibility.AC3.3 Success:** CC plugin's agents translated to spawn configs; invokable via the plugin's declared interface
- **v3-extensibility.AC3.4 Success:** CC plugin's monitors translated to Port implementations; subscribable via `ctx.port.subscribe()`
- **v3-extensibility.AC3.6 Success:** CC compatibility Haskell library included in agent prelude; maps CC terminology to pattern terminology

---

## Tasks

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: CC agents → `EphemeralConfig` translation

**Verifies:** AC3.3.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/cc_adapter/agents.rs` — agent frontmatter parser + `EphemeralConfig` builder.

**Implementation:**

CC `agents/<name>.md` files have YAML frontmatter mirroring Anthropic's documented agent shape: `name`, `description`, `tools` (allowed tool list), `model` (optional model override), plus body containing the agent's system prompt or program.

```rust
use std::path::Path;
use serde::Deserialize;
use pattern_core::spawn::EphemeralConfig;
use pattern_core::traits::plugin::{PluginContext, PluginError};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcAgentFrontmatter {
    pub name: String,
    #[serde(default)] pub description: Option<String>,
    #[serde(default)] pub tools: Vec<String>,
    #[serde(default)] pub model: Option<String>,
    /// Pattern-specific opt-in. When "draft", a draft persona KDL is written
    /// via RuntimeConfigWriter for later human-promote. Default: in-memory only.
    #[serde(default)] pub persona_mode: Option<PersonaMode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersonaMode { Ephemeral, Draft }

#[derive(Debug, Clone)]
pub struct CcAgentTemplate {
    pub name: smol_str::SmolStr,
    pub description: Option<String>,
    pub allowed_tools: Vec<smol_str::SmolStr>,
    pub model: Option<smol_str::SmolStr>,
    pub program_body: String,
    pub persona_mode: PersonaMode,
}

pub fn translate_agents(plugin_root: &Path, manifest: &PluginManifest)
    -> Result<Vec<CcAgentTemplate>, PluginError>
{
    // Walk <plugin_root>/agents/*.md (or paths from manifest.agents).
    // Parse frontmatter + body. Build CcAgentTemplate per file.
}

impl CcAgentTemplate {
    /// Construct an in-memory EphemeralConfig for spawning this CC agent.
    pub fn to_ephemeral_config(&self) -> EphemeralConfig {
        let mut config = EphemeralConfig::new(self.program_body.clone());
        if let Some(desc) = &self.description {
            config = config.with_costume(desc.clone());
        }
        if !self.allowed_tools.is_empty() {
            // Translate CC tool names (Pattern's effect categories or specific tools)
            // into a CapabilitySet restriction.
            let cap = capabilities_from_cc_tools(&self.allowed_tools);
            config = config.with_capabilities(cap);
        }
        // model field is informational; spawn handler resolves provider per session.
        config
    }
}
```

`capabilities_from_cc_tools` maps CC tool names (`Read`, `Write`, `Edit`, `Bash`, `WebFetch`, etc.) to Pattern `EffectCategory` flags. CC names that don't map (e.g., `WebFetch` if Pattern doesn't expose it) produce a tracing-warn at translation time and are silently dropped from the capability set.

The CC adapter stores `Vec<CcAgentTemplate>` on `LoadedPlugin` (extend `LoadedPlugin` with `cc_agent_templates: Vec<CcAgentTemplate>` — defaults to empty for native plugins). On agent invocation (Partner-typed `/agent <plugin>:<name>` or Agent-spawn `Pattern.Cc.spawnAgent(...)`), the runtime resolves the template and calls `to_ephemeral_config()`.

When `persona_mode == Draft`, on_install also calls `RuntimeConfigWriter::write_kdl(...)` to materialize a draft persona at `<drafts_dir>/<plugin-id>--<agent-name>.kdl`.

**Testing:**
Tests must verify each AC listed above:
- AC3.3: Install fixture CC plugin with `agents/refactorer.md` declaring tools `[Read, Edit]`. Assert `LoadedPlugin.cc_agent_templates` contains the template; assert `template.to_ephemeral_config()` produces a config whose `capabilities` restricts to Memory + File effects only.
- Edge: Agent frontmatter with `persona_mode: draft` triggers a write at `<drafts_dir>/<plugin-id>--<agent-name>.kdl` with the agent's system prompt as the persona's `system_prompt`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter::agents`.

**Commit:** `[pattern-runtime] translate CC agents to in-memory EphemeralConfig`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: CC monitors → `MonitorPort` impl

**Verifies:** AC3.4.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/cc_adapter/monitors.rs` — monitor parser + `MonitorPort`.

**Implementation:**

CC `monitors/<name>.json` declares a long-running command whose stdout is treated as an event stream. Shape:

```json
{
    "name": "git-status",
    "description": "fires on git working-tree changes",
    "command": "fswatch -0 -e .git ${CLAUDE_PLUGIN_ROOT}",
    "interval_ms": null
}
```

```rust
use async_trait::async_trait;
use futures::stream::BoxStream;
use pattern_core::traits::port::{Port, PortCapabilities, PortError, PortEvent, PortMetadata};
use pattern_core::types::port::PortId;

#[derive(Debug)]
pub struct MonitorPort {
    plugin_id: smol_str::SmolStr,
    plugin_root: PathBuf,
    monitor: CcMonitorSpec,
    /// Single live process per port. Spawned at first subscribe; reused on resubscribe.
    state: parking_lot::Mutex<MonitorState>,
}

#[derive(Debug, Default)]
struct MonitorState {
    process: Option<tokio::process::Child>,
    line_rx: Option<tokio::sync::broadcast::Sender<String>>,
}

#[async_trait]
impl Port for MonitorPort {
    fn id(&self) -> &PortId { /* "<plugin-id>:monitor:<monitor-name>" */ }

    fn metadata(&self) -> PortMetadata { /* description + tags */ }

    fn capabilities(&self) -> PortCapabilities {
        PortCapabilities::default().with_subscribable(true)
    }

    async fn subscribe(&self, _config: serde_json::Value) -> Result<BoxStream<'static, PortEvent>, PortError> {
        // Lazy-spawn the monitor process if not running. Use tokio::process::Command.
        // Pipe stdout into a broadcast channel; subscribers convert broadcast::Receiver
        // to a Stream<Item = PortEvent::Line { content }>.
        // Substitute ${CLAUDE_PLUGIN_ROOT} in the command.
        // Return BoxStream.
    }

    async fn call(&self, _method: &str, _payload: serde_json::Value) -> Result<serde_json::Value, PortError> {
        Err(PortError::NotSupported { method: "call".into(), reason: "monitors are subscribe-only".into() })
    }
}
```

Process lifecycle:
- Spawn on first subscribe. Reuse for additional subscribers on the same port (broadcast channel fan-out).
- Tear down on the CC adapter's `on_disable`. Each `MonitorPort` keeps a handle to its process; the adapter holds Arc references and aborts on disable.
- Stderr is logged at `tracing::warn` per line (debug if quiet-mode in config).
- Crashes: if the process exits unexpectedly, emit a `PortEvent::Closed { reason }` and tear down. Subscribers that re-subscribe trigger a respawn.

Registration: at `CcPluginAdapter::on_enable`, walk `LoadedPlugin.cc_monitors` (extend `LoadedPlugin` with `cc_monitors: Vec<CcMonitorSpec>`), construct one `MonitorPort` per monitor, register into the per-session `PortRegistryImpl::register(...)`. Capture the `PortId`s in `AdapterState` for `on_disable` to unregister.

**Testing:**
Tests must verify each AC listed above:
- AC3.4: Install fixture CC plugin with `monitors/echo-once.json` (`command: "echo hello"`). Enable the adapter. Subscribe via `port_registry.dispatcher.subscribe(port_id, {})`. Assert the first `PortEvent::Line` contains `"hello"`. After process exits, subsequent subscribe respawns.
- Edge: Disable the adapter → assert process is killed (use a long-running fixture monitor like `sleep 60` and check process.kill via pid).

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter::monitors`.

**Commit:** `[pattern-runtime] translate CC monitors to MonitorPort impls`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->

<!-- START_TASK_3 -->
### Task 3: `CommandRegistry` foundation in `pattern_server`

**Verifies:** None directly (infrastructure for Task 4).

**Files:**
- Create: `crates/pattern_server/src/commands/mod.rs` (root + re-exports).
- Create: `crates/pattern_server/src/commands/registry.rs` (`CommandRegistry`, `CommandHandler` trait).
- Create: `crates/pattern_core/src/commands.rs` (`CommandResponse`, `CommandResponseKind`, `CommandSideEffect`, `CommandAudience` types).
- Modify: `crates/pattern_server/src/protocol.rs:353` — change `RunCommand` reply from `String` to `CommandResponse`.
- Modify: `crates/pattern_server/src/server.rs:958-969` — replace placeholder dispatch with registry lookup.
- Modify: `crates/pattern_cli` callers of `run_command` to consume `CommandResponse` instead of `String`.

**Implementation:**

In `pattern_core::commands`:

```rust
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CommandResponse {
    pub content: String,                       // Markdown rendered in TUI / agent context.
    pub kind: CommandResponseKind,
    pub side_effects: Vec<CommandSideEffect>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommandResponseKind {
    /// TUI display only — not added to turn history.
    DisplayOnly,
    /// Inserted as a system-origin message in turn history.
    SystemMessage,
    /// Promoted into a user-typed prompt on the agent's next turn.
    UserMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CommandSideEffect {
    PersonaSwitched { to: SmolStr },
    ModelChanged { to: SmolStr },
    BlockEdited { label: SmolStr, scope: SmolStr },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommandAudience {
    /// Invocable only by Partner-authored input (typed in TUI).
    Partner,
    /// Invocable by agent SDK calls (e.g., from a Haskell program).
    Agent,
    /// Both.
    Both,
    /// Internal — not reachable from external dispatch (used by other plugin code).
    Internal,
}
```

In `pattern_server::commands`:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashMap;

use pattern_core::commands::{CommandAudience, CommandResponse};
use pattern_core::types::origin::Author;

#[async_trait]
pub trait CommandHandler: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn audience(&self) -> CommandAudience;
    async fn handle(&self, args: &[String], origin: &Author) -> Result<CommandResponse, CommandError>;
}

#[derive(Debug, Default)]
pub struct CommandRegistry {
    inner: RwLock<HashMap<SmolStr, Arc<dyn CommandHandler>>>,
}

impl CommandRegistry {
    pub fn register(&self, handler: Arc<dyn CommandHandler>) -> Result<(), CommandError> {
        // Reject duplicate names with CommandError::AlreadyRegistered.
    }
    pub fn unregister(&self, name: &str) -> bool { /* ... */ }
    pub async fn dispatch(&self, name: &str, args: &[String], origin: &Author) -> Result<CommandResponse, CommandError> {
        // 1. Look up handler.
        // 2. Audience-tier gate via origin.
        // 3. Delegate to handler.handle(args, origin).
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CommandError {
    #[error("command not found: {name}")]
    NotFound { name: String },
    #[error("command already registered: {name}")]
    AlreadyRegistered { name: String },
    #[error("command {name} requires {required:?} audience but caller is {actual:?}")]
    AudienceDenied { name: String, required: CommandAudience, actual: SmolStr },
    #[error("command {name} handler failed: {message}")]
    HandlerFailed { name: String, message: String },
}
```

`DaemonServer` gains `command_registry: Arc<CommandRegistry>`. The `RunCommand` RPC handler dispatches via `command_registry.dispatch(...)` and returns the `CommandResponse`.

The CLI consumes `CommandResponse` and renders by `kind`:
- `DisplayOnly` → render `content` in TUI immediately, no history insert.
- `SystemMessage` → insert as a system-attributed message in the visible history; doesn't trigger a turn.
- `UserMessage` → submits `content` as a Partner-authored prompt; triggers a turn.

**Testing:**
Tests must verify the registry surface:
- Dispatch unknown command → `NotFound`.
- Register two handlers with same name → second registration returns `AlreadyRegistered`.
- Audience gate: Partner-only command invoked with `Author::Agent(_)` → `AudienceDenied`.
- Successful dispatch returns the handler's `CommandResponse` unmodified.

**Verification:**
Run: `cargo nextest run -p pattern-server commands::`.

**Commit:** `[meta] [pattern-core] [pattern-server] [pattern-cli] add CommandRegistry + CommandResponse + audience-tier dispatch`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: CC commands → registered slash commands

**Verifies:** Part of AC3 — commands are not in AC3.1-3.8 but are an explicit Phase 2 design component the implementation plan rolls into Phase 4. Document as "covered by Phase 4 done-when checklist."

**Files:**
- Create: `crates/pattern_runtime/src/plugin/cc_adapter/commands.rs` — parser + `CcCommandHandler` impl.

**Implementation:**

CC `commands/<name>.md` files have YAML frontmatter (audience, description, tools/skills allowed) plus markdown body. The body is the command's behavior — typically a templated prompt or instructions for the runtime.

```rust
use serde::Deserialize;
use pattern_core::commands::{CommandAudience, CommandResponse, CommandResponseKind};
use pattern_server::commands::CommandHandler;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcCommandFrontmatter {
    pub name: String,
    #[serde(default)] pub description: Option<String>,
    #[serde(default = "default_audience")] pub audience: CommandAudience,
    /// Body interpretation. "literal" returns the body as-is; "template" performs
    /// {{argN}} substitution; "user_message" treats the body as a prompt template.
    #[serde(default = "default_body_kind")] pub body_kind: CcCommandBodyKind,
    /// What kind of CommandResponse the body produces.
    #[serde(default = "default_response_kind")] pub response_kind: CommandResponseKind,
}

fn default_audience() -> CommandAudience { CommandAudience::Partner }
fn default_body_kind() -> CcCommandBodyKind { CcCommandBodyKind::Literal }
fn default_response_kind() -> CommandResponseKind { CommandResponseKind::DisplayOnly }

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CcCommandBodyKind { Literal, Template, UserMessage }

#[derive(Debug)]
pub struct CcCommandHandler {
    plugin_id: smol_str::SmolStr,
    name: String,
    audience: CommandAudience,
    body: String,
    body_kind: CcCommandBodyKind,
    response_kind: CommandResponseKind,
}

#[async_trait]
impl CommandHandler for CcCommandHandler {
    fn name(&self) -> &str { &self.name }
    fn audience(&self) -> CommandAudience { self.audience }

    async fn handle(&self, args: &[String], _origin: &Author) -> Result<CommandResponse, CommandError> {
        let content = match self.body_kind {
            CcCommandBodyKind::Literal => self.body.clone(),
            CcCommandBodyKind::Template => render_template(&self.body, args),
            CcCommandBodyKind::UserMessage => render_template(&self.body, args), // same expansion
        };
        Ok(CommandResponse {
            content,
            kind: self.response_kind,
            side_effects: Vec::new(),
        })
    }
}

fn render_template(template: &str, args: &[String]) -> String {
    // Replace {{1}}, {{2}}, ..., {{n}} with args[0..n-1]; drop unmatched placeholders.
    // Replace {{*}} with args.join(" ").
}
```

Registration at `CcPluginAdapter::on_enable`:

```rust
for cmd_template in &self.cc_command_templates {
    let handler = Arc::new(CcCommandHandler {
        plugin_id: self.plugin_id.clone(),
        name: format!("{}:{}", self.plugin_id, cmd_template.name), // namespaced: <plugin-id>:<cmd>
        audience: cmd_template.audience,
        body: cmd_template.body.clone(),
        body_kind: cmd_template.body_kind,
        response_kind: cmd_template.response_kind,
    });
    ctx.command_registry.register(handler)?;
}
```

Note the namespacing: every CC plugin's command is exposed as `/<plugin-id>:<command-name>` to avoid collisions across plugins. The TUI's slash autocomplete will surface these prefixed names.

**Testing:**
Tests must verify each AC listed above:
- Install CC fixture plugin with `commands/hello.md` declaring `audience: partner`, `body_kind: literal`, body `"hello world"`. Enable adapter. Dispatch `/<plugin-id>:hello` via `CommandRegistry::dispatch(name, [], &Author::Partner(...))`. Assert response.content is `"hello world"`.
- Audience-tier gate: same command dispatched with `Author::Agent(_)` returns `CommandError::AudienceDenied`.
- Template: `commands/echo.md` with `body_kind: template`, body `"echoed: {{*}}"`, args `["foo", "bar"]` → response.content `"echoed: foo bar"`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter::commands`.

**Commit:** `[pattern-runtime] translate CC commands to CommandRegistry handlers`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: CC `bin/` → session PATH augmentation

**Verifies:** None directly (infrastructure consumed by AC3 integration test).

**Files:**
- Modify: `crates/pattern_runtime/src/plugin/cc_adapter.rs` — extend `AdapterState` with `path_additions: Vec<PathBuf>`.
- Modify: `crates/pattern_runtime/src/session.rs::open_with_agent_loop` — accept a `path_additions: Vec<PathBuf>` parameter and pass to `ProcessManager::with_env`.
- Modify: `crates/pattern_server/src/server.rs::get_or_open_session` — collect path additions from enabled CC plugins.

**Implementation:**

At `CcPluginAdapter::on_install`, check for `<plugin_root>/bin/`. If exists, store on `LoadedPlugin.cc_bin_path: Option<PathBuf>`. At `on_enable`, the adapter records the path on its `AdapterState`. The session-open helper aggregates `cc_bin_path` across all enabled CC plugins for that session.

`ProcessManager::with_env` is the integration point:

```rust
// in pattern_server::server::get_or_open_session
let path_additions: Vec<PathBuf> = mount.enabled_cc_plugin_bins();    // aggregate across plugins
let process_manager = ProcessManager::new(cwd, cache_dir).with_env(build_path_env(&path_additions));

// build_path_env prepends path_additions to existing $PATH:
fn build_path_env(additions: &[PathBuf]) -> HashMap<String, String> {
    let existing = std::env::var("PATH").unwrap_or_default();
    let prefix = additions.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(":");
    let new_path = if prefix.is_empty() { existing } else { format!("{prefix}:{existing}") };
    HashMap::from([("PATH".into(), new_path)])
}
```

**Testing:**
- Install fixture CC plugin with `bin/hello-plugin` (a small script). Open session against the mount with that plugin enabled. Run `Shell.Execute("hello-plugin")` and assert it resolves and runs (output captured).
- Disable plugin: re-open session, run again, assert `command not found`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter::path`.

**Commit:** `[pattern-runtime] [pattern-server] inject CC plugin bin/ into session PATH`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->

<!-- START_TASK_6 -->
### Task 6: Normalize CC `.mcp.json` → `pattern_core::mcp::McpServerConfig`

**Verifies:** Part of AC3 staging for Phase 5; AC5 fully verified there.

**Files:**
- Create: `crates/pattern_core/src/mcp.rs` — `McpServerConfig`, `McpTransport` types.
- Create: `crates/pattern_runtime/src/plugin/cc_adapter/mcp_config.rs` — parser + translator.

**Implementation:**

`pattern_core::mcp` defines a transport-agnostic config the Phase 5 loader consumes:

```rust
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct McpServerConfig {
    pub name: SmolStr,
    pub transport: McpTransport,
    pub env: BTreeMap<String, String>,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpTransport {
    Stdio { command: String, args: Vec<String>, cwd: Option<PathBuf> },
    Http { url: String, headers: BTreeMap<String, String> },
    Sse { url: String, headers: BTreeMap<String, String> },
}
```

The CC adapter parser at `cc_adapter/mcp_config.rs`:

```rust
pub fn parse_mcp_servers(plugin_root: &Path, manifest: &PluginManifest)
    -> Result<Vec<McpServerConfig>, PluginError>
{
    // Source priority:
    //   1. manifest.mcp_servers ComponentSpec::Inline(json)  (CC inlines in plugin.json)
    //   2. manifest.mcp_servers ComponentSpec::Path(path)    (CC points at .mcp.json)
    //   3. <plugin_root>/.mcp.json                           (CC default location)
    // Each .mcp.json shape:
    //   { "mcpServers": { "<name>": { "command": "...", "args": [...], "env": {...} } } }
    // Translate each entry to McpServerConfig:
    //   - "command" present → Stdio
    //   - "url" present → Http (or Sse if "transport": "sse" set)
}
```

Stored on `LoadedPlugin.cc_mcp_servers: Vec<McpServerConfig>` (extend in this task). At `on_enable`, the adapter doesn't *spawn* MCP servers — that's Phase 5's job. It registers the configs into a per-session `Vec<McpServerConfig>` accumulator on `SessionContext` for Phase 5 to consume at session open.

**Testing:**
- Parse a fixture `.mcp.json` with one stdio + one http entry. Assert two `McpServerConfig` entries with correct transport variants.
- Variant: inline `mcpServers` in `plugin.json` (preserved into `manifest.cc.fields["mcpServers"]` by Phase 1) parses identically.
- Edge: missing `.mcp.json` → returns empty Vec, not an error.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter::mcp_config`.

**Commit:** `[pattern-core] [pattern-runtime] add McpServerConfig + CC .mcp.json normalizer`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: `Pattern.Cc` Haskell compatibility module + conditional prelude inclusion

**Verifies:** AC3.6.

**Files:**
- Create: `crates/pattern_runtime/haskell/Pattern/Cc.hs` — terminology re-exports + idiom shims + hook event aliases.
- Modify: `crates/pattern_runtime/src/sdk/preamble.rs::build_with_libraries` — accept conditional inclusion flag for `Pattern.Cc`.
- Modify: `crates/pattern_runtime/src/session.rs::open_with_agent_loop` — pass `cc_plugin_enabled` flag to preamble builder.
- Modify: `crates/pattern_server/src/server.rs::get_or_open_session` — set `cc_plugin_enabled = enabled_plugins.iter().any(|p| p.manifest.cc.is_some())`.

**Implementation:**

`Pattern.Cc.hs` is a single Haskell module compiled into the prelude when CC plugins are enabled. Initial scope (~80 lines):

```haskell
{-# LANGUAGE FlexibleContexts, NoImplicitPrelude #-}
-- | Pattern.Cc — Claude Code compatibility layer.
--
-- Maps CC terminology to Pattern semantics for agent code written in CC dialect.
-- Imported automatically when at least one CC plugin is enabled in the session.
module Pattern.Cc
    ( -- * Terminology re-exports (CC-style names → Pattern modules)
      tool, useTool
    , skill, invokeSkill
    , agent, spawnAgent
      -- * Hook event aliases (CC event names → Pattern hook tags as constants)
    , preToolUse, postToolUse, sessionStart, sessionEnd, taskCompleted
      -- * Frontmatter idiom shims
    , withClaudePluginRoot, hookPayload
    ) where

import Pattern.Prelude
import qualified Pattern.Skills as Skills
import qualified Pattern.Spawn as Spawn
import qualified Pattern.Memory as Memory
import qualified Pattern.Display as Display
import qualified Pattern.Log as Log

-- | Invoke a skill by name. CC dialect uses "tool" and "skill" interchangeably
-- for shell-style helpers; this maps to Pattern's skill load.
useTool :: Member Skills effs => Text -> [Text] -> Eff effs Text
useTool name args = Skills.load name >> pure ("invoked " <> name)

invokeSkill :: Member Skills effs => Text -> [Text] -> Eff effs Text
invokeSkill = useTool

-- | Spawn an ephemeral CC agent by name. Resolves the agent template from the
-- enabled CC plugins; fails at runtime with a clear error if no plugin declares
-- an agent of that name.
spawnAgent :: Member Spawn effs => Text -> Text -> Eff effs Spawn.SpawnId
spawnAgent agentName initialPrompt =
    Spawn.ephemeral (Spawn.ephemeralConfigByName agentName initialPrompt)

-- | Hook tag constants (string-typed; subscribers use HookFilter.new).
preToolUse, postToolUse, sessionStart, sessionEnd, taskCompleted :: Text
preToolUse     = "tool.before"
postToolUse    = "tool.after"
sessionStart   = "persona.attached"
sessionEnd     = "persona.detached"
taskCompleted  = "task.transitioned.done"

-- | Substitute ${CLAUDE_PLUGIN_ROOT} in a path string. Useful in CC-dialect
-- programs that hard-code the substitution token.
withClaudePluginRoot :: Member Memory effs => Text -> Eff effs Text
withClaudePluginRoot s = do
    root <- Memory.get "system/cc_plugin_root"  -- runtime injects this block
    pure (replaceAll "${CLAUDE_PLUGIN_ROOT}" (contentText root) s)
  where
    replaceAll _ _ s = s  -- (sketch; real impl uses Text.replace)

-- | Decode hook payload JSON to a value. Hook handlers in CC dialect typically
-- read $PATTERN_HOOK_PAYLOAD; this provides the equivalent in-program.
hookPayload :: Member Memory effs => Eff effs (Maybe Aeson.Value)
hookPayload = do
    raw <- Memory.get "system/hook_payload"
    pure (Aeson.decodeText (contentText raw))

-- ... terse aliases for common idioms; ~50 lines total
```

Conditional inclusion in preamble:

```rust
// pattern_runtime::sdk::preamble
pub fn build_with_libraries(
    decls: &[EffectDecl],
    port_libraries: &[(PortId, &str)],
    cc_compat_enabled: bool,
) -> String {
    let mut out = String::new();
    // ... existing pragma / imports / type alias ...
    if cc_compat_enabled {
        out.push_str("\nimport qualified Pattern.Cc as Cc\n");
    }
    out
}
```

The `Pattern.Cc.hs` source is materialized into the per-session prelude tempdir at session open, alongside port library sources. `pattern_runtime` ships the `.hs` file as an embedded `&'static str` via `include_str!`.

**Testing:**
Tests must verify each AC listed above:
- AC3.6: Open a session with one enabled CC plugin; assert `Pattern.Cc` is on the include path; compile a test agent program that imports `Pattern.Cc` and uses `Cc.preToolUse`. Assert compilation succeeds.
- Negative: open a session with no CC plugins; assert `Pattern.Cc` is NOT on the include path; importing it from an agent program produces a `module not found` compile error.
- Edge: agent program uses both `Pattern.Cc` aliases and direct `Pattern.Skills` calls; both compile and resolve.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter::haskell_compat` (uses `compile_and_run` against tidepool-extract).

**Commit:** `[pattern-runtime] add Pattern.Cc compatibility module + conditional prelude inclusion`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (task 8) -->

<!-- START_TASK_8 -->
### Task 8: AC3.3 / AC3.4 / AC3.6 integration suite

**Verifies:** AC3.3, AC3.4, AC3.6 — end-to-end.

**Files:**
- Create: `crates/pattern_runtime/tests/plugin_cc_adapter_full.rs`.
- Create: `crates/pattern_runtime/tests/fixtures/plugins/cc-full-fixture/` — CC plugin with agents/, monitors/, commands/, bin/, .mcp.json.

**Implementation:**

```rust
#[tokio::test]
async fn cc_full_plugin_translates_all_artifact_kinds() {
    let env = TestEnv::new().await;
    install_and_enable(&env, "cc-full-fixture").await;
    let plugin = env.registry.get("cc-full-fixture").unwrap();

    // AC3.3: agents present as in-memory templates
    assert_eq!(plugin.cc_agent_templates.len(), 1);
    let cfg = plugin.cc_agent_templates[0].to_ephemeral_config();
    assert!(cfg.capabilities.is_some());

    // AC3.4: monitors registered as ports
    let port_id = format!("cc-full-fixture:monitor:tick-once");
    let port = env.port_registry.get(&port_id.into()).unwrap();
    let mut stream = port.subscribe(serde_json::Value::Null).await.unwrap();
    use futures::StreamExt;
    let first = stream.next().await.unwrap();
    assert!(matches!(first, PortEvent::Line { .. }));

    // commands registered + dispatchable
    let resp = env.command_registry.dispatch(
        "cc-full-fixture:hello",
        &[],
        &Author::Partner(test_partner()),
    ).await.unwrap();
    assert_eq!(resp.content, "hello world");

    // bin/ on PATH (smoke check)
    let exec_resp = env.process_manager.execute(
        env.tempdir.path(),
        "hello-plugin",
        Default::default(),
        Duration::from_secs(5),
    ).await.unwrap();
    assert!(exec_resp.exit_code.success());

    // .mcp.json normalized
    assert_eq!(plugin.cc_mcp_servers.len(), 1);

    // AC3.6: Pattern.Cc compiled into prelude
    let session = env.open_session_for_plugin("cc-full-fixture").await;
    let agent_program = r#"
        import qualified Pattern.Cc as Cc
        agent = pure Cc.preToolUse
    "#;
    let result = session.compile_haskell(agent_program).await;
    assert!(result.is_ok(), "Pattern.Cc compile failed: {:?}", result.err());
}
```

The fixture plugin layout:
```
cc-full-fixture/
├── .claude-plugin/plugin.json    (name: "cc-full-fixture")
├── agents/refactorer.md          (frontmatter + body)
├── monitors/tick-once.json       ({command: "echo tick"})
├── commands/hello.md             (frontmatter audience:partner, body "hello world")
├── bin/hello-plugin              (executable script)
└── .mcp.json                     ({mcpServers: {test: {command: "echo"}}})
```

**Testing:** the integration test above is the proof.

**Verification:**
Run: `cargo nextest run -p pattern-runtime --test plugin_cc_adapter_full`.

**Commit:** `[pattern-runtime] add CC adapter full-surface integration suite`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_D -->

---

## Phase done-when checklist

- [ ] CC agents parse and translate to in-memory `EphemeralConfig`; `pattern.persona_mode = "draft"` opt-in writes draft KDL.
- [ ] CC monitors parse and register as `MonitorPort` impls in the per-session port registry; subscribe/respawn/cleanup work.
- [ ] `CommandRegistry` foundation in `pattern_server` works: registration, dispatch, audience-tier gating, `RunCommand` RPC switched to `CommandResponse` return.
- [ ] CC commands parse with their own frontmatter (audience + body kind + response kind) and register as namespaced (`<plugin-id>:<cmd>`) handlers.
- [ ] CC `bin/` directory aggregated across enabled plugins is prepended to session PATH.
- [ ] CC `.mcp.json` parses to `pattern_core::mcp::McpServerConfig` collection on `LoadedPlugin`; staged for Phase 5 consumption.
- [ ] `Pattern.Cc.hs` ships in the runtime; conditionally spliced into prelude only when CC plugins are enabled.
- [ ] AC3.3, AC3.4, AC3.6 integration suite green.
- [ ] `cargo nextest run --workspace` green; no existing tests regress.
- [ ] `cargo fmt` + `cargo clippy --all-features --all-targets` clean.

---

## Notes for executor

- **The slash command infrastructure is greenfield.** Do not stub it. The `RunCommand` RPC currently returns `"not yet implemented"`; Phase 4 swaps it for the real registry. Per-project-guidance: this is fix-the-stub work, not stub-around-it work.
- **`CommandResponse` wire change is breaking.** Update CLI consumers in the same commit; do not leave `String` paths around.
- **Monitor process lifecycle is the trickiest piece.** Watch for: zombie processes on adapter disable, ${CLAUDE_PLUGIN_ROOT} substitution edge cases, broadcast channel buffer-fill behavior with slow subscribers. Use the existing `LocalPtyBackend` patterns from sandbox-io as reference, NOT a fresh tokio::process abstraction.
- **`Pattern.Cc` initial scope is small (~80 lines).** Don't preemptively expand it. The plan ships hook tag constants + skill/agent/tool aliases + `withClaudePluginRoot`. If a real CC plugin during Phase 8 smoke testing reveals a missing alias, add it then.
- **Audience-tier per command comes from the command's own frontmatter.** Default is Partner. Internal-tier commands (audience: internal) register but never dispatch via `CommandRegistry::dispatch` — they're invoked directly by other plugin code through a separate `CommandRegistry::dispatch_internal` method (not exposed to RPC).
- **CC plugin command namespace prefix.** Use `<plugin-id>:<command-name>`. Avoids collisions across plugins. Document in TUI completion logic (Phase 7 / followup).
- **No coupling to Phase 5's MCP loader.** Phase 4 only normalizes config + stages it. The `cc_mcp_servers: Vec<McpServerConfig>` field on `LoadedPlugin` is the contract Phase 5 consumes.
- **Per project guidance:** if implicit work surfaces (e.g., `LoadedPlugin` shape changes need to be threaded across all earlier-phase callers), do the threading. Don't shim.
- **Future work signposted, not stubbed:** the question of whether `pattern_core::mcp::*` should grow to host the full MCP client implementation is a Phase 5 design decision. Phase 4 only adds the config types. Don't preempt.
