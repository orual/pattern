# v3-extensibility Phase 3: Plugin trait boundary + CC adapter shell + skills

**Goal:** `PluginExtension` and `PluginHost` traits in `pattern_core::traits::plugin`. `Author::Plugin` variant added to the origin enum. `CcPluginAdapter` wraps CC plugin directories as `PluginExtension` impls. CC `SKILL.md` translation into Pattern Skill blocks with `SkillTrustTier::PluginInstalled` (activates the Plan-2 reservation). CC `command`-type and `http`-type hooks dispatch via Phase 2's `HookBus` using the CC alias table for tag translation and a payload-level tool matcher. `LoadedPlugin` extended to carry the extension trait object plus an optional host trait object for plugins that make callbacks.

**Architecture:** Plugin code path through the runtime is uniform: `PluginRegistry` from Phase 1 holds `LoadedPlugin { ..., extension: Arc<dyn PluginExtension>, host: Option<Arc<dyn PluginHost>> }`. The `PluginHost` trait is the typed surface for plugin → runtime callbacks (memory access, send_message, tasks, skills, skill_invoke). It mirrors `PluginProtocol`'s host-callback variants method-for-method. Two real implementations: `RuntimePluginHost` (in `pattern_runtime`, wraps the actual MemoryStore/mailbox/task registry handles) and `IrpcPluginHost` (in `pattern-plugin-sdk`, wraps an `irpc::Client<PluginProtocol>` so plugin authors get typed method calls that route over the wire). CC adapter sets `host: None` — CC plugins never make host callbacks (pure event-driven, no autonomous behavior). Plugin authors access via `ctx.host()? -> &dyn PluginHost` (Err if `host` is None for the plugin kind). Memory access is layered on top: `PluginMemorySync` (Phase 6) is a `MemoryStore`-shaped client that internally calls `host.memory_*` methods and maintains a local CRDT cache; `ctx.memory()? -> Arc<PluginMemorySync>` returns it for plugins that want the trait-shaped surface. CC plugins resolved at install time when `manifest.cc` is `Some` — Phase 1's manifest carries the residue, Phase 3's `CcPluginAdapter::wrap(loaded_plugin, plugin_root)` constructs the trait object. Skill translation walks `<plugin>/skills/<name>/SKILL.md`, reuses the existing `pattern_memory::fs::markdown_skill::parse` (saphyr-backed; no new YAML parser), decorates the resulting `SkillMetadata` with `trust_tier: PluginInstalled` and `source_plugin_id: Some(plugin.id)`. Hook bindings: at `on_enable`, the adapter walks the CC manifest's `hooks` declarations, looks up each CC event name in `cc_aliases::translate_cc(...)`, and registers a notification subscriber on the resulting Pattern tag. The subscriber's callback applies the CC tool matcher against the event payload, then dispatches the hook handler — `command` hooks shell out via `ProcessManager::execute(...)` with `cwd = plugin_root`; `http` hooks POST via `HttpPort::post(...)`. `prompt`/`agent`/`mcp_tool` hook types are recognized but skipped with a warning at `on_enable` (they land in later phases or follow-ups).

**Tech Stack:** `#[async_trait]` (for select async methods on `PluginExtension::on_install`/`on_enable`/`on_disable`), `Arc<dyn Trait>` for trait objects, existing `pattern_memory::fs::markdown_skill::parse` (saphyr-backed; already used by the runtime's skill-load path) for SKILL.md frontmatter, `globset 0.4` (added Phase 2) for tool matcher compilation.

**Scope:** 3 of 7 phases.

**Codebase verified:** 2026-04-27.

---

## Codebase verification findings

- ✓ `crates/pattern_core/src/traits/` has 10 trait modules; `port.rs` is the canonical async-trait shape — `#[async_trait] pub trait Port: Send + Sync + Debug { ... }`.
- ✓ `SkillTrustTier::PluginInstalled` is a live enum variant at `crates/pattern_core/src/types/memory_types/skill.rs:96-110`. Currently unreached — Phase 3 activates it.
- ✓ `SkillMetadata` at `skill.rs:59-78` lacks plugin-origin tracking. Phase 3 adds `source_plugin_id: Option<SmolStr>`. Field is `Option`, so existing on-disk skill blocks deserialize unchanged via `#[serde(default)]`.
- ✓ Skill load path at `crates/pattern_runtime/src/sdk/handlers/skills.rs:171` hard-codes `trust_tier: SkillTrustTier::ProjectLocal`. The runtime path stays unchanged; the CC adapter path bypasses this site (skills come from the adapter directly into the registry).
- ✓ `ProcessManager::execute(cwd, command, env, timeout)` from v3-sandbox-io Phase 3 — sync execution, output capture, exit-code return. Suitable for CC `command` hook shell-out.
- ✓ `HttpPort` from v3-sandbox-io Phase 4 — `get`/`post`/`put`/`patch`/`delete` methods. Suitable for CC `http` hook POSTing.
- ✓ Phase 2's `HookBus::subscribe_notifications(filter) -> (id, mpsc::Receiver<HookEvent>)`. CC adapter consumes this surface; receivers drained on a per-plugin tokio task spawned at `on_enable`.
- ✓ `Arc<dyn Port>` and similar trait-object patterns established. `Arc<dyn PluginExtension>` follows.
- ✓ `Message.Ask` GADT exists at `crates/pattern_runtime/haskell/Pattern/Message.hs:67` but handler is **stubbed as candidate-for-removal** (`sdk/handlers/message.rs:92-95`). CC `prompt`-type hooks would map here cleanly, but un-stubbing requires its own focused plan — deferred.
- ✓ SKILL.md parser already exists at `crates/pattern_memory/src/fs/markdown_skill/parse.rs::parse(bytes: &[u8]) -> Result<SkillFile, SkillParseError>`. Uses `saphyr 0.0.6` (existing workspace dep). Returns `SkillFile { metadata: SkillMetadata, extras: LoroValue, body: String }`. Phase 3 reuses this — no new parser, no new dep. **Note: `serde_yaml` is unmaintained; `saphyr` is the project-blessed YAML crate.**

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-extensibility.AC3: CC plugin adapter (subset; Phase 4 covers AC3.3/3.4/3.6, Phase 6 covers AC3.8)

- **v3-extensibility.AC3.1 Success:** CC-format plugin wrapped by `CcPluginAdapter`; adapter implements `PluginExtension`; runtime manages it identically to native plugins
- **v3-extensibility.AC3.2 Success:** CC plugin's skills translated to Skill blocks with `trust_tier: PluginInstalled`; visible via `ctx.skills.list()`
- **v3-extensibility.AC3.5 Success:** CC plugin's hooks dispatch through `on_event()` with CC event alias mapping (e.g., `PreToolUse` → `tool.before`)
- **v3-extensibility.AC3.7 Failure:** CC plugins do not declare any host-callback resources in their manifest (CC plugins have no callback concept); accessing host-callback surfaces from a CC adapter context returns `PluginError::NotDeclared { resource }` with a clear message. *(Reinterpreted: design plan said "PluginHost methods return NotSupported"; with the dropped `PluginHost` trait, the equivalent semantic is "CC adapter's `PluginContext` has no callback resources declared, so accessor methods like `ctx.memory()` return NotDeclared".)*

### v3-extensibility.AC7: Trust enforcement (subset)

- **v3-extensibility.AC7.1 Success:** Skills from installed plugins receive `trust_tier: PluginInstalled` via the code path reserved in Plan 2

---

## Tasks

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: `PluginExtension` trait + `PluginContext` + `Author::Plugin` + supporting types

**Verifies:** None (infrastructure; consumed by Tasks 3-7).

**Files:**
- Create: `crates/pattern_core/src/traits/plugin.rs` (root: declares submodules + re-exports).
- Create: `crates/pattern_core/src/traits/plugin/extension.rs` (`PluginExtension` trait).
- Create: `crates/pattern_core/src/traits/plugin/extension.rs` (`PluginExtension` trait).
- Create: `crates/pattern_core/src/traits/plugin/host.rs` (`PluginHost` trait — methods mirror `PluginProtocol`'s host-callback variants from Phase 6 Task 3 wire types).
- Create: `crates/pattern_core/src/traits/plugin/types.rs` (`PluginContext`, `PortDeclaration`, `PluginError`).
- Modify: `crates/pattern_core/src/types/origin.rs` — add `Author::Plugin { plugin_id, partner_authority }` variant; update `bypasses_permission_gate()`.
- Modify: `crates/pattern_core/src/traits.rs` — declare `pub mod plugin;`.

**Implementation:**

`PluginExtension` is the runtime-facing trait. Lifecycle methods are async (plugin code may await network/IO during install/enable). Event dispatch (`on_event`) is sync — it operates against an already-extracted `HookEvent` payload and dispatches to subscribers; any async work the plugin does runs inside its own tokio task spawned at `on_enable`.

```rust
use async_trait::async_trait;
use std::sync::Arc;

use crate::types::port::PortId;

/// Plugin trait. Every plugin — native IRPC, CC adapter, MCP adapter — implements this.
#[async_trait]
pub trait PluginExtension: Send + Sync + std::fmt::Debug {
    /// What this plugin provides.
    fn ports(&self) -> Vec<PortDeclaration> { Vec::new() }

    /// Optional Haskell library text spliced into agent prelude when the plugin is enabled.
    fn library(&self) -> Option<&'static str> { None }

    /// Lifecycle: install. Called once when the plugin is added to the registry.
    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError> { let _ = ctx; Ok(()) }

    /// Lifecycle: enable. Called when the plugin is bound to a session/runtime context.
    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError> { let _ = ctx; Ok(()) }

    /// Lifecycle: disable. Called when the plugin is detached or the session ends.
    async fn on_disable(&self, ctx: &PluginContext) -> Result<(), PluginError> { let _ = ctx; Ok(()) }

    /// Hook event handler. Called by the runtime when a HookEvent matches one of the
    /// plugin's registered tag globs. Returns `Some(HookResponse)` for blocking events
    /// (the bus will respect Block/Modify); `None` for notification events.
    fn on_event(&self, event: &HookEvent) -> Option<HookResponse> { let _ = event; None }
}
```

`PluginHost` is the runtime → plugin callback trait. Method signatures mirror `PluginProtocol`'s host-callback variants one-for-one — same shape across in-process and out-of-process. Two real impls (Phase 6 lands them): `RuntimePluginHost` wraps the runtime's actual handles; `IrpcPluginHost` wraps an `irpc::Client<PluginProtocol>` and routes calls over the wire. CC adapter holds `host: None` (CC plugins never callback).

```rust
use async_trait::async_trait;

use pattern_core::traits::plugin::wire::{
    BlockAddr, WireHostMessage, WireSearchQuery, WireSearchResult,
    WireSkillInvoke, WireSkillInvocation, WireTaskCreate, WireTaskTransition,
    WireTaskLink, WireTaskQuery, WireTaskItem, WireArchivalEntry,
};
use pattern_core::types::memory_types::{
    BlockCreate, BlockMetadata, BlockMetadataPatch, BlockFilter, UndoRedoOp,
};
use pattern_core::types::ids::{MessageId, TaskItemId};

#[async_trait]
pub trait PluginHost: Send + Sync + std::fmt::Debug {
    // Memory ops that genuinely round-trip (db-poking; non-loro):
    async fn memory_create_block(&self, create: BlockCreate) -> Result<BlockMetadata, PluginError>;
    async fn memory_delete_block(&self, addr: BlockAddr) -> Result<(), PluginError>;
    async fn memory_search(&self, query: WireSearchQuery) -> Result<Vec<WireSearchResult>, PluginError>;
    async fn memory_list_blocks(&self, filter: BlockFilter) -> Result<Vec<BlockMetadata>, PluginError>;
    async fn memory_persist(&self, addr: BlockAddr) -> Result<(), PluginError>;
    async fn memory_update_metadata(&self, addr: BlockAddr, patch: BlockMetadataPatch) -> Result<(), PluginError>;
    async fn memory_undo_redo(&self, addr: BlockAddr, op: UndoRedoOp) -> Result<bool, PluginError>;
    async fn memory_get_shared_block(&self, owner: SmolStr, label: SmolStr) -> Result<Option<BlockMetadata>, PluginError>;
    async fn memory_insert_archival(&self, entry: WireArchivalEntry) -> Result<(), PluginError>;
    async fn memory_search_archival(&self, query: WireSearchQuery) -> Result<Vec<WireArchivalEntry>, PluginError>;
    async fn memory_delete_archival(&self, id: SmolStr) -> Result<(), PluginError>;

    // Cross-domain search (working + archival; no `memory` scope required)
    async fn search(&self, query: WireSearchQuery) -> Result<Vec<WireSearchResult>, PluginError>;

    // Messaging
    async fn send_message(&self, msg: WireHostMessage) -> Result<MessageId, PluginError>;

    // Tasks
    async fn task_create(&self, req: WireTaskCreate) -> Result<TaskItemId, PluginError>;
    async fn task_transition(&self, req: WireTaskTransition) -> Result<(), PluginError>;
    async fn task_link(&self, req: WireTaskLink) -> Result<(), PluginError>;
    async fn task_query(&self, req: WireTaskQuery) -> Result<Vec<WireTaskItem>, PluginError>;

    // Skills
    async fn skill_invoke(&self, req: WireSkillInvoke) -> Result<WireSkillInvocation, PluginError>;
}
```

Note: every method signature uses the wire types defined in `pattern_core::traits::plugin::wire` (Phase 6 Task 3). This keeps the trait directly serializable across the IRPC boundary — `IrpcPluginHost`'s methods literally call `self.client.rpc(...)` for each one. `RuntimePluginHost`'s methods unwrap the wire types into runtime-internal types, dispatch into the scoped `MemoryStore` wrapper / mailbox / task registry / etc.

**No `NoOpPluginHost` stub** — earlier drafts proposed one for adapters that don't need a host. Replaced with `host: Option<Arc<dyn PluginHost>>` on `LoadedPlugin`: CC adapter sets `None`, `IrpcPluginHost` is `Some`. Cleaner — plugin code that calls `ctx.host()?` gets a structured `PluginError::HostUnavailable` if the plugin kind doesn't make callbacks, no NotSupported-per-method per-stub returns.

`types.rs` defines:

```rust
use std::path::PathBuf;
use std::sync::Arc;
use smol_str::SmolStr;

use crate::capability::CapabilitySet;
use crate::traits::memory_store::MemoryStore;
use crate::types::port::PortId;
use crate::hooks::HookBus;

/// Context handed to plugin lifecycle methods. Owns the handles a plugin
/// needs to call back to the runtime. Concrete implementation is in
/// `pattern_runtime` (in-process) or `pattern_plugin_sdk` (out-of-process,
/// IRPC-backed).
#[derive(Debug)]
pub struct PluginContext {
    pub plugin_id: SmolStr,
    pub plugin_root: PathBuf,
    pub user_config: serde_json::Value,
    pub effective_capabilities: CapabilitySet,
    pub hook_bus: Arc<HookBus>,
    /// Host callback surface. None for CC adapter; Some for native IRPC plugins.
    /// Plugin authors access via `ctx.host()`.
    pub(crate) host: Option<Arc<dyn PluginHost>>,
    /// MemoryStore-shaped client. Layered on top of `host` — `PluginMemorySync`
    /// (Phase 6) calls `host.memory_*` methods internally and maintains a
    /// local CRDT cache. None when manifest didn't declare `requires { memory }`
    /// or user didn't grant.
    pub(crate) memory_sync: Option<Arc<PluginMemorySync>>,
}

impl PluginContext {
    /// Get the host surface. Err if plugin kind doesn't make callbacks
    /// (e.g., CC adapter).
    pub fn host(&self) -> Result<&Arc<dyn PluginHost>, PluginError> {
        self.host.as_ref().ok_or(PluginError::HostUnavailable)
    }

    /// Plugin's MemoryStore-shaped client. Subscribe/apply_delta available
    /// under the `memory-sync` SDK feature. Err if memory scope not declared
    /// in manifest or not granted by user.
    pub fn memory(&self) -> Result<Arc<PluginMemorySync>, PluginError> {
        self.memory_sync.clone().ok_or(PluginError::NotDeclared { resource: "memory" })
    }
}

#[derive(Debug, Clone)]
pub struct PortDeclaration {
    pub id: PortId,
    pub description: String,
    pub library: Option<smol_str::SmolStr>,    // SmolStr (per Port trait swap in Phase 6 Task 1). Field name matches Port::library() trait method.
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    /// Plugin kind has no host callback surface (e.g., CC adapter — pure
    /// event-driven, never calls back).
    #[error("plugin host callbacks not available for this plugin kind")]
    HostUnavailable,

    /// A resource (memory, etc.) wasn't declared in the plugin manifest, so
    /// the corresponding accessor isn't available.
    #[error("plugin did not declare {resource:?} in its manifest")]
    NotDeclared { resource: &'static str },

    /// User hasn't granted a declared resource (manifest-declared but
    /// blocked at install / runtime config).
    #[error("plugin {plugin_id}: {resource:?} declared but not granted")]
    NotGranted { plugin_id: SmolStr, resource: &'static str },

    /// Permission broker denied a specific operation.
    #[error("plugin {plugin_id}: {operation} denied: {reason}")]
    PermissionDenied { plugin_id: SmolStr, operation: &'static str, reason: String },

    #[error("plugin {plugin_id} install failed: {message}")]
    InstallFailed { plugin_id: SmolStr, message: String },

    #[error("plugin {plugin_id} hook handler failed: {message}")]
    HookHandlerFailed { plugin_id: SmolStr, message: String },

    #[error("plugin {plugin_id} skill translation failed at {path}: {message}")]
    SkillTranslationFailed { plugin_id: SmolStr, path: PathBuf, message: String },

    #[error("plugin {plugin_id} transport error: {message}")]
    TransportLost { plugin_id: SmolStr, message: String },

    #[error("plugin {plugin_id} subprocess died: {message}")]
    ProcessDied { plugin_id: SmolStr, message: String },
}
```

**Plugin identity.** Phase 3 also extends `pattern_core::types::origin::Author` with a new variant:

```rust
#[non_exhaustive]
pub enum Author {
    Partner(Partner),
    Agent(AgentAuthor),
    Plugin {
        plugin_id: smol_str::SmolStr,
        /// True only when the plugin was granted `partner_authority` scope at
        /// install AND user explicitly enabled it. False = autonomous plugin
        /// activity runs at intersection-of-capabilities (plugin ∩ recipient).
        partner_authority: bool,
    },                                   // NEW
    Human(Human),
    System(SystemReason),
}
```

Update `Author::bypasses_permission_gate()` to return `true` for `Plugin { partner_authority: true, .. }` (matches the documented semantics from earlier discussion).

**Verification:**
Run: `cargo check -p pattern-core`.

**Commit:** `[pattern-core] add PluginExtension trait + PluginContext + PluginError + Author::Plugin variant`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Extend `SkillMetadata` with `source_plugin_id`

**Verifies:** AC7.1 (provenance side; trust-tier side covered in Task 4).

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/skill.rs:59-78` — add `source_plugin_id` field.
- Modify: `crates/pattern_runtime/src/sdk/handlers/skills.rs:171` — set `source_plugin_id: None` for the runtime/Memory.Put path.
- Modify: any other site that constructs `SkillMetadata` directly (search via `grep -n "SkillMetadata {"` across `crates/`).

**Implementation:**

```rust
// in pattern_core::types::memory_types::skill
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SkillMetadata {
    pub name: String,
    pub trust_tier: SkillTrustTier,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub hooks: serde_json::Value,
    /// Plugin id when `trust_tier == PluginInstalled`; otherwise None.
    /// `#[serde(default)]` so existing on-disk skill blocks deserialize unchanged.
    #[serde(default)]
    pub source_plugin_id: Option<smol_str::SmolStr>,
}
```

Update every direct construction site to populate the field. For the Memory.Put-driven AdHoc path and the runtime's ProjectLocal load, `source_plugin_id` is `None`.

**Testing:**
- Round-trip: serialize a `SkillMetadata` without `source_plugin_id`, deserialize via the new shape; field defaults to `None`.
- Forward-compat: serialize with `Some("test-plugin".into())`, deserialize, assert preserved.

**Verification:**
Run: `cargo nextest run -p pattern-core skill` and `cargo nextest run -p pattern-runtime skills`.

**Commit:** `[pattern-core] [pattern-runtime] add source_plugin_id to SkillMetadata`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->

<!-- START_TASK_3 -->
### Task 3: `CcPluginAdapter` scaffold + lifecycle

**Verifies:** AC3.1, AC3.7.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/cc_adapter.rs` (root).
- Create: `crates/pattern_runtime/src/plugin/cc_adapter/lifecycle.rs` (on_install / on_enable / on_disable).
- Modify: `crates/pattern_runtime/src/plugin.rs` — declare `pub mod cc_adapter;` and re-export.
- Modify: `crates/pattern_runtime/Cargo.toml` — add `pattern-memory = { path = "../pattern_memory" }` if not already present (Task 4 reuses its SKILL.md parser).

**Implementation:**

```rust
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::task::JoinHandle;

use pattern_core::traits::plugin::{
    PluginContext, PluginError, PluginExtension, PortDeclaration,
};

use crate::plugin::manifest::PluginManifest;

#[derive(Debug)]
pub struct CcPluginAdapter {
    plugin_id: smol_str::SmolStr,
    plugin_root: PathBuf,
    manifest: PluginManifest,
    /// Spawned at on_enable, aborted at on_disable. Drains hook subscription receivers.
    state: RwLock<AdapterState>,
}

#[derive(Debug, Default)]
struct AdapterState {
    hook_drain_tasks: Vec<JoinHandle<()>>,
    enabled: bool,
}

impl CcPluginAdapter {
    pub fn wrap(plugin_id: smol_str::SmolStr, plugin_root: PathBuf, manifest: PluginManifest) -> Arc<Self> {
        Arc::new(Self {
            plugin_id,
            plugin_root,
            manifest,
            state: RwLock::new(AdapterState::default()),
        })
    }

    // No host method — CC plugins do not make host callbacks. The PluginContext
    // they receive at lifecycle methods has no resource accessors declared
    // (per their empty `requires { ... }` block), so any accidental call to
    // ctx.memory() / ctx.search() / etc. returns NotDeclared.
}

#[async_trait]
impl PluginExtension for CcPluginAdapter {
    fn ports(&self) -> Vec<PortDeclaration> {
        // CC monitors translate to ports; that lands in Phase 4. For Phase 3, no ports.
        Vec::new()
    }

    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Translate skills (delegated to Task 4's translator).
        crate::plugin::cc_adapter::skills::install_skills(&self.plugin_id, &self.plugin_root, &self.manifest, ctx).await?;
        Ok(())
    }

    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Wire hook subscriptions (Task 5).
        let tasks = crate::plugin::cc_adapter::hooks::wire_hook_subscriptions(self, ctx).await?;
        let mut state = self.state.write();
        state.hook_drain_tasks = tasks;
        state.enabled = true;
        Ok(())
    }

    async fn on_disable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        let mut state = self.state.write();
        for task in state.hook_drain_tasks.drain(..) {
            task.abort();
        }
        state.enabled = false;
        Ok(())
    }

    fn on_event(&self, _event: &HookEvent) -> Option<HookResponse> {
        // CC adapter does its dispatch via subscription receivers, not by being directly
        // called. The HookBus pushes events to receivers spawned in on_enable. on_event
        // here is for IRPC plugins that want centralized dispatch — CC doesn't.
        None
    }
}
```

**Testing:**
- Construct adapter, assert lifecycle methods round-trip.
- Verify a CC plugin's `PluginContext` (received at on_install) has no host-callback resources declared — call `ctx.memory()` and assert `PluginError::NotDeclared { resource: "memory" }`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter`.

**Commit:** `[pattern-runtime] add CcPluginAdapter scaffold + lifecycle methods`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: SKILL.md parser + skill translator

**Verifies:** AC3.2, AC7.1.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/cc_adapter/skills.rs`.

**Implementation:**

**Reuse existing parser.** `pattern_memory::fs::markdown_skill::parse::parse(bytes: &[u8]) -> Result<SkillFile, SkillParseError>` already parses `SKILL.md` files (frontmatter + body) using `saphyr`. It returns:

```rust
pub struct SkillFile {
    pub metadata: SkillMetadata,    // typed: name, trust_tier, description, keywords, hooks
    pub extras: LoroValue,          // unknown frontmatter keys preserved as Map for round-trip
    pub body: String,
}
```

The parser already populates a `SkillMetadata` value. Phase 3 uses it as-is, then **decorates** with plugin provenance. Do NOT write a second YAML parser.

`skills.rs` translates skill files into Pattern Skill blocks:

```rust
use std::path::Path;
use pattern_core::traits::plugin::{PluginContext, PluginError};
use pattern_core::types::memory_types::skill::{SkillMetadata, SkillTrustTier};
use crate::plugin::manifest::PluginManifest;

pub async fn install_skills(
    plugin_id: &smol_str::SmolStr,
    plugin_root: &Path,
    manifest: &PluginManifest,
    ctx: &PluginContext,
) -> Result<(), PluginError> {
    // 1. Determine skill source paths from manifest:
    //    - `manifest.skills` (resolved component spec) → list of paths
    //    - Default: <plugin_root>/skills/ if manifest declared no skills field
    let skill_dirs = resolve_skill_dirs(plugin_root, manifest);

    // 2. Walk each <skills_dir>/<name>/SKILL.md:
    for skill_dir in skill_dirs {
        for entry in std::fs::read_dir(&skill_dir).map_err(|e| translate_io(plugin_id, &skill_dir, e))? {
            let entry = entry.map_err(|e| translate_io(plugin_id, &skill_dir, e))?;
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() { continue; }

            let raw = std::fs::read(&skill_md)
                .map_err(|e| translate_io(plugin_id, &skill_md, e))?;

            // Reuse the existing saphyr-backed parser from pattern_memory.
            let mut parsed = pattern_memory::fs::markdown_skill::parse::parse(&raw)
                .map_err(|e| PluginError::SkillTranslationFailed {
                    plugin_id: plugin_id.clone(),
                    path: skill_md.clone(),
                    message: e.to_string(),
                })?;

            // Decorate metadata: PluginInstalled trust tier + source attribution.
            // Parser sets a tier already (likely AdHoc-default for a freshly-parsed file);
            // we override here because we KNOW the source is a plugin install.
            parsed.metadata.trust_tier = SkillTrustTier::PluginInstalled;
            parsed.metadata.source_plugin_id = Some(plugin_id.clone());

            // Persist as a Skill block via the runtime's memory store. PluginContext
            // (Task 1) needs a `memory_store: Arc<dyn MemoryStore>` field — added here
            // if not already present from Task 1. Use the same path as the runtime's
            // Memory.Put handler:
            persist_skill_block(ctx, parsed.metadata, parsed.extras, parsed.body).await?;
        }
    }
    Ok(())
}
```

**Note for executor:** `PluginContext` from Task 1 doesn't currently carry a `MemoryStore` handle. Add it. The PluginContext field gains:

```rust
pub memory_store: Arc<dyn MemoryStore>,
```

Wire from `LoadedPlugin` / registry into the context at on_install / on_enable call sites.

**Testing:**
Tests must verify each AC listed above:
- AC3.2: Install fixture CC plugin with two skills; assert both Skill blocks materialize via `MemoryStore::list_blocks(SkillSchema)`; each carries `trust_tier: PluginInstalled` and `source_plugin_id: Some("fixture-plugin".into())`.
- AC7.1: Same as above — the trust tier assertion is the AC7.1 verification.

Negative tests:
- SKILL.md missing frontmatter returns `SkillTranslationFailed` with the file path.
- Frontmatter missing `name` field returns `SkillTranslationFailed`.

Fixture: `crates/pattern_runtime/tests/fixtures/plugins/cc-fixture/skills/foo/SKILL.md` and `bar/SKILL.md`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter::skills`.

**Commit:** `[pattern-runtime] translate CC plugin skills to Skill blocks via existing markdown_skill parser`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: CC hook subscription wiring (`command` + `http`)

**Verifies:** AC3.5.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/cc_adapter/hooks.rs`.

**Implementation:**

CC hooks are declared in the manifest's `hooks` ComponentSpec — either as inline JSON (CC plugin.json) or at `<plugin_root>/hooks/hooks.json`. Each entry has shape:

```json
{
    "matcher": "Write|Edit",     // tool-name regex/glob
    "hooks": [
        { "type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/scripts/check.sh", "event": "PreToolUse" },
        { "type": "http", "url": "https://example/hook", "event": "PostToolUse" }
    ]
}
```

(Phase 1's manifest preserved this in `manifest.hooks` as `Vec<ComponentSpec>` with inline JSON.)

```rust
use std::sync::Arc;
use globset::{Glob, GlobMatcher};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use pattern_core::hooks::{HookBus, HookEvent, HookFilter, cc_aliases};
use pattern_core::traits::plugin::{PluginContext, PluginError};

use super::CcPluginAdapter;

pub async fn wire_hook_subscriptions(
    adapter: &CcPluginAdapter,
    ctx: &PluginContext,
) -> Result<Vec<JoinHandle<()>>, PluginError> {
    let mut tasks = Vec::new();

    let hook_decls = parse_cc_hook_declarations(&adapter.manifest, &adapter.plugin_root)
        .map_err(|e| PluginError::HookHandlerFailed { plugin_id: adapter.plugin_id.clone(), message: e.to_string() })?;

    for decl in hook_decls {
        let pattern_tag = match cc_aliases::translate_cc(&decl.event) {
            Some(t) => t,
            None => {
                warn!(plugin = %adapter.plugin_id, cc_event = %decl.event, "unknown CC event name; hook skipped");
                continue;
            }
        };

        // Filter the bus on the pattern tag (e.g., "tool.before").
        let filter = HookFilter::new(pattern_tag).map_err(|e| {
            PluginError::HookHandlerFailed { plugin_id: adapter.plugin_id.clone(), message: format!("invalid hook filter: {e}") }
        })?;

        let (sub_id, mut rx) = ctx.hook_bus.subscribe_notifications(filter);

        // Compile the tool matcher (CC's `matcher` field) once.
        let tool_matcher = match decl.matcher.as_deref() {
            Some(pattern) => Some(compile_tool_matcher(pattern)?),
            None => None,
        };

        let handler = decl.handler.clone(); // {Command{cmd, env}, Http{url, method}, Skipped(reason)}
        let plugin_id = adapter.plugin_id.clone();
        let plugin_root = adapter.plugin_root.clone();

        let task = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if !matcher_passes(&tool_matcher, &event) {
                    debug!(?event.tag, "tool matcher rejected event");
                    continue;
                }
                match &handler {
                    HookHandler::Command { command, env } => {
                        if let Err(e) = run_command_hook(&plugin_id, &plugin_root, command, env, &event).await {
                            warn!(?e, "command hook execution failed");
                        }
                    }
                    HookHandler::Http { url, method, headers } => {
                        if let Err(e) = run_http_hook(url, method.as_deref(), headers, &event).await {
                            warn!(?e, "http hook execution failed");
                        }
                    }
                    HookHandler::Skipped { reason, original_type } => {
                        debug!(?original_type, ?reason, "hook type not yet supported; skipping");
                    }
                }
            }
            debug!(plugin = %plugin_id, sub_id, "hook subscription receiver closed");
        });

        tasks.push(task);
    }

    Ok(tasks)
}

fn run_command_hook(
    plugin_id: &str,
    plugin_root: &std::path::Path,
    command: &str,
    env: &std::collections::BTreeMap<String, String>,
    event: &HookEvent,
) -> impl std::future::Future<Output = Result<(), HookExecError>> {
    async move {
        let process_manager = /* obtained from PluginContext or runtime registry */;
        let payload_json = serde_json::to_string(&event.payload).unwrap_or_default();
        let mut env = env.clone();
        env.insert("PATTERN_HOOK_PAYLOAD".into(), payload_json);
        env.insert("PATTERN_HOOK_TAG".into(), event.tag.to_string());
        env.insert("CLAUDE_PLUGIN_ROOT".into(), plugin_root.display().to_string());

        // Execute with cwd = plugin_root (matches CC convention, confirmed by user).
        let outcome = process_manager.execute(plugin_root, command, env, std::time::Duration::from_secs(30)).await?;
        if !outcome.exit_code.success() {
            return Err(HookExecError::NonZeroExit { command: command.to_string(), exit_code: outcome.exit_code, stderr: outcome.stderr });
        }
        Ok(())
    }
}
```

`run_http_hook` POSTs the payload via `HttpPort::post(...)`. Use the runtime's already-instantiated `HttpPort` from the port registry — the adapter looks it up by id `"http"` at on_enable.

For unsupported handler types (`prompt`, `agent`, `mcp_tool`), the parser produces `HookHandler::Skipped { reason, original_type }` and the dispatcher logs a debug line per event.

`compile_tool_matcher` translates CC's `Write|Edit` regex-like syntax into a globset pattern (CC's matcher uses pipe-separated literals in practice — implement a small parser that accepts `A|B|C` and emits `{A,B,C}` glob set, plus passthrough for `*` wildcards).

`matcher_passes` extracts the `tool_name` from the event payload (for `tool.before`/`tool.after` events) and returns true if the matcher matches.

**Testing:**
Tests must verify each AC listed above:
- AC3.5: Install CC fixture plugin with `PreToolUse` hook on matcher `Write`. Trigger a `tool.before` HookEvent with tool_name="Write"; assert the command hook ran (use a script that writes a marker file). Trigger another `tool.before` with tool_name="Read"; assert no command ran.
- Negative: Hook with unsupported type (`prompt`) emits the warn-level debug log, doesn't crash.
- Edge: Adapter with no hook declarations returns empty task list.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::cc_adapter::hooks`.

**Commit:** `[pattern-runtime] add CC hook subscription wiring (command + http)`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->

<!-- START_TASK_6 -->
### Task 6: Extend `LoadedPlugin` + dispatch CC adapter at install

**Verifies:** AC3.1.

**Files:**
- Modify: `crates/pattern_runtime/src/plugin/registry.rs` — extend `LoadedPlugin` with `extension` and `host` fields; add `install` dispatching by manifest source format.

**Implementation:**

```rust
use pattern_core::traits::plugin::{PluginExtension, PluginHost};

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub id: PluginId,
    pub scope: PluginScope,
    pub source_path: PathBuf,
    pub manifest: PluginManifest,
    pub user_config: serde_json::Value,
    pub capability_overrides: Option<CapabilitySet>,
    pub extension: Arc<dyn PluginExtension>,
    /// None for CC adapter (CC plugins make no callbacks); Some for native
    /// IRPC plugins (Phase 6 wires the IrpcPluginHost concrete impl).
    pub host: Option<Arc<dyn PluginHost>>,
}

impl PluginRegistry {
    /// Construct the appropriate adapter based on manifest source format.
    /// Returns (extension, optional host).
    fn build_extension(plugin_id: &PluginId, source_path: &Path, manifest: &PluginManifest)
        -> (Arc<dyn PluginExtension>, Option<Arc<dyn PluginHost>>)
    {
        if manifest.cc.is_some() {
            let adapter = CcPluginAdapter::wrap(plugin_id.clone(), source_path.to_path_buf(), manifest.clone());
            // CC adapter has no host — pure event-driven, never calls back.
            (adapter as Arc<dyn PluginExtension>, None)
        } else {
            // Phase 6 will add the IRPC native adapter (which DOES populate host).
            // For Phase 3, native plugins fall back to a deferred-marker adapter that
            // warns at on_enable. host stays None until Phase 6 wires it.
            (native_stub_adapter(plugin_id, source_path, manifest), None)
        }
    }
}
```

The `native_stub_adapter` returns a `NativeStubAdapter` that only logs `"native IRPC plugin transport not yet wired (Phase 6)"` at `on_enable` and otherwise does nothing — explicit deferred-with-marker rather than silently broken. Phase 6's task that introduces `IrpcPluginHost` updates `build_extension` to construct it for native plugins.

Wire into the install path from Task 7 of Phase 1:

```rust
pub fn install(&self, source: InstallSource<'_>, target_scope: PluginScope, jj: &JjAdapter) -> Result<PluginId, RegistryError> {
    // ... existing Phase 1 staging + cache-rename code ...
    let manifest = load_manifest(&final_dir)?;
    let (extension, host) = Self::build_extension(&manifest.name, &final_dir, &manifest);
    let lp = LoadedPlugin {
        id: manifest.name.clone(),
        scope: target_scope,
        source_path: final_dir,
        manifest,
        user_config: serde_json::Value::Null,
        capability_overrides: None,
        extension,
        host,
    };
    // ... insert + persist KDL ...
    // Call extension.on_install(...).await — but install is currently sync. Either:
    //  (a) Make install async (most callers already async).
    //  (b) Use tokio::runtime::Handle::current().block_on(...) — bridge.
    // (a) is cleaner; PluginRegistry::install becomes async fn. Phase 1's tests use #[tokio::test].
}
```

**Testing:**
- Install CC-format plugin (manifest.cc.is_some). Assert `loaded.extension.type_id()` matches CC adapter; assert `loaded.host.is_none()` (CC plugins make no callbacks).
- Install Pattern-native (manifest.cc.is_none). Assert NativeStubAdapter; assert on_enable logs the deferred-marker warning; assert `loaded.host.is_none()` (Phase 6 wires the real `IrpcPluginHost`).

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::registry`.

**Commit:** `[pattern-runtime] dispatch CC adapter at plugin install + extend LoadedPlugin`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: AC3 + AC7.1 integration tests

**Verifies:** AC3.1, AC3.2, AC3.5, AC3.7, AC7.1 — end-to-end.

**Files:**
- Create: `crates/pattern_runtime/tests/plugin_cc_adapter.rs`.
- Create: `crates/pattern_runtime/tests/fixtures/plugins/cc-adapter-fixture/` — CC plugin layout with a manifest, two skills, and one PreToolUse `command` hook.

**Implementation:**

Integration suite:

```rust
#[tokio::test]
async fn cc_plugin_install_translates_skills_with_plugin_installed_tier() {
    let env = TestEnv::new().await;
    env.registry.install(
        InstallSource::LocalPath(Path::new("tests/fixtures/plugins/cc-adapter-fixture")),
        PluginScope::Global,
        &env.jj,
    ).await.unwrap();

    let plugin = env.registry.get("cc-adapter-fixture").unwrap();
    plugin.extension.on_install(&env.plugin_context()).await.unwrap();

    let skills = env.memory_store.list_blocks(BlockSchema::Skill).await.unwrap();
    let foo = skills.iter().find(|s| s.metadata.name == "foo").unwrap();
    assert_eq!(foo.metadata.trust_tier, SkillTrustTier::PluginInstalled);          // AC7.1
    assert_eq!(foo.metadata.source_plugin_id.as_deref(), Some("cc-adapter-fixture"));
}

#[tokio::test]
async fn cc_plugin_pretooluse_hook_dispatches_with_matcher_filtering() {
    let env = TestEnv::new().await;
    install_and_enable(&env, "cc-adapter-fixture").await;

    let marker = env.tempdir.path().join("hook_fired.marker");
    let event = HookEvent {
        tag: tags::TOOL_BEFORE.into(),
        payload: serde_json::json!({ "tool_name": "Write", "marker_path": marker.display().to_string() }),
        metadata: HookEventMetadata::for_test(),
        semantics: HookSemantics::Notification,
    };
    env.hook_bus.emit(event);

    wait_for_file(&marker, Duration::from_secs(5)).await;     // AC3.5
}

#[tokio::test]
async fn cc_plugin_host_returns_not_supported() {
    let env = TestEnv::new().await;
    install_and_enable(&env, "cc-adapter-fixture").await;
    let plugin = env.registry.get("cc-adapter-fixture").unwrap();

    // CC adapter sets host=None. Plugin-context's host() accessor returns
    // PluginError::HostUnavailable for any callback attempt.
    assert!(plugin.host.is_none());
    let ctx = env.plugin_context_for(&plugin);
    let err = ctx.host().unwrap_err();
    assert!(matches!(err, PluginError::HostUnavailable));                       // AC3.7

    // Equivalent assertion via memory accessor: CC plugin doesn't declare
    // memory scope in manifest, so ctx.memory() returns NotDeclared.
    let err2 = ctx.memory().unwrap_err();
    assert!(matches!(err2, PluginError::NotDeclared { resource: "memory" }));
}

#[tokio::test]
async fn cc_plugin_routes_through_pluginextension_uniformly() {
    let env = TestEnv::new().await;
    install_and_enable(&env, "cc-adapter-fixture").await;
    let plugin = env.registry.get("cc-adapter-fixture").unwrap();

    // The runtime's plugin path treats every LoadedPlugin uniformly via Arc<dyn PluginExtension>.
    // Calling plugin.extension.on_install / on_enable / on_disable through the trait object works.
    assert!(plugin.extension.ports().is_empty());      // CC adapter declares no ports (Phase 3)
    assert!(plugin.extension.library().is_none());     // No Haskell lib in Phase 3 (Phase 4 adds CC compat lib)
                                                        // AC3.1
}
```

**Testing:**
The four tests above cover the AC matrix end-to-end. Helpers (`TestEnv`, `install_and_enable`, `wait_for_file`) live alongside in the test module.

The fixture plugin's `hooks/hooks.json` declares one PreToolUse hook with matcher `"Write"` and a script that touches a marker file at `${PATTERN_HOOK_PAYLOAD}`-derived path. Script lives at `tests/fixtures/plugins/cc-adapter-fixture/scripts/touch_marker.sh`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime --test plugin_cc_adapter`.

**Commit:** `[pattern-runtime] add CC adapter integration suite for AC3.1/3.2/3.5/3.7 + AC7.1`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase done-when checklist

- [ ] `pattern_core::traits::plugin` exposes `PluginExtension`, `PluginHost`, `PluginContext`, `PortDeclaration`, `PluginError`. `Author::Plugin { plugin_id, partner_authority }` variant added; `bypasses_permission_gate()` updated.
- [ ] `SkillMetadata` carries `source_plugin_id: Option<SmolStr>`.
- [ ] CC adapter reuses `pattern_memory::fs::markdown_skill::parse::parse(...)` for SKILL.md frontmatter (no new YAML parser, no new dep).
- [ ] `CcPluginAdapter` implements `PluginExtension` end-to-end; lifecycle methods round-trip; CC plugin's `LoadedPlugin.host` is `None`; `ctx.host()` returns `PluginError::HostUnavailable`; `ctx.memory()` returns `PluginError::NotDeclared { resource: "memory" }`.
- [ ] CC SKILL.md frontmatter parses and translates to Skill blocks with `trust_tier: PluginInstalled` + `source_plugin_id: Some(plugin.id)`.
- [ ] CC `command` and `http` hooks dispatch via `HookBus::subscribe_notifications` with payload-level tool matcher; CC alias map applied at registration.
- [ ] `prompt`, `agent`, `mcp_tool` hook types parse without crashing and emit a debug-level "skipped" log per event.
- [ ] `LoadedPlugin` carries `extension: Arc<dyn PluginExtension>` + `host: Option<Arc<dyn PluginHost>>` (CC sets None; Phase 6 wires `IrpcPluginHost` for native plugins); `PluginRegistry::install` dispatches CC vs native at install.
- [ ] All AC3.1, AC3.2, AC3.5, AC3.7, AC7.1 cases pass under `cargo nextest run -p pattern-runtime --test plugin_cc_adapter`.
- [ ] `cargo nextest run --workspace` green.
- [ ] `cargo fmt` + `cargo clippy --all-features --all-targets` clean.

---

## Notes for executor

- **`prompt`-type hooks emit a deferred-marker, not a stub.** When the parser encounters `{ type: "prompt", ... }`, it produces `HookHandler::Skipped { reason: "prompt-type hooks deferred to future Message.Ask plan", original_type: "prompt" }`. The dispatcher logs at debug. **Do not silently drop**: a CC plugin author needs to know the hook didn't fire.
- **Same applies to `agent` and `mcp_tool`**: skipped-with-marker, with reason citing the phase that will land them (Phase 4 and Phase 5 respectively).
- **CC matcher syntax is permissive in the wild.** `compile_tool_matcher` should accept: literal strings (`Write`), pipe-separated alternatives (`Write|Edit`), CC's `Bash(git *)` shape (tool name + arg pattern). For Phase 3, support literal + pipe-alternatives + bare wildcard `*`. Anything more exotic produces `HookHandler::Skipped { reason: "unsupported matcher syntax: <pattern>" }`. Document this as a known limitation in the phase notes.
- **`NativeStubAdapter` in Task 6 is the explicit deferred-marker for IRPC native plugins.** Logs a warn-level "native IRPC plugin transport not yet wired (Phase 6)" line at on_enable. **Do not stub silently** — the warn line is the contract.
- **`PluginContext.memory_store` extension in Task 4** is implicit work flagged here for the executor: Phase 1's PluginContext shape is sufficient for hook events but doesn't carry a memory store. Add the field; thread from `LoadedPlugin` through `PluginRegistry::call_lifecycle(...)` (whatever the on_install caller is named).
- **Per project guidance:** if any of the implicit work above (matcher coverage, PluginContext extension, lifecycle async transition in Task 6) reveals deeper architectural questions, surface them — don't stub or shortcut.
- **Test fixture skills should be minimal.** SKILL.md with name + description is enough for AC3.2. Don't pad fixtures.
- **Keep CC adapter trait-object-safe.** `CcPluginAdapter` as `Arc<dyn PluginExtension>` is the load-bearing test. If something requires `&mut self` on a method that's hot, reach for `parking_lot::RwLock` inside `self`, not method signature changes.
