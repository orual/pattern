# v3-extensibility Phase 1: Plugin manifest and registry

**Goal:** KDL-native `PluginManifest` parsing, CC `plugin.json` parsing + translation into `PluginManifest` (residue under `cc {}`), `PluginRegistry` with split shared/private project pinning, project > global > ambient precedence, install (local-path copy or jj-driven git clone) and uninstall ops.

**Architecture:** Single `PluginManifest` Rust type, two parsing entry points, **structured preservation of unknowns in both formats** (no opaque string blobs). Pattern-native `manifest.kdl` parses via the `kdl` crate's `KdlDocument` directly (NOT knus's `Decode` derive — knus is strict-by-design and lacks a flatten/preserve escape hatch; we want forward-compat). The parser walks top-level nodes, dispatches known node names to typed extractors, and collects unknown nodes into `PluginManifest.unknown_kdl: BTreeMap<String, kdl::KdlNode>`. CC `.claude-plugin/plugin.json` decodes via `serde_json` into a transient DTO with `#[serde(flatten)] extra: BTreeMap<String, serde_json::Value>` to capture unknowns; a translator function then maps known CC fields to Pattern equivalents and packs `extra` plus explicitly-preserved fields into a structured `Cc { source_format: SmolStr, fields: BTreeMap<String, serde_json::Value> }` block on `PluginManifest`. Both formats round-trip; later phases (Phase 3 CC adapter, Phase 4 CC completion) consume `Cc.fields` as typed JSON, NOT a re-parsed string. `PluginRegistry` persists pin state (scope + per-plugin user-config tunables + capability overrides) to KDL files via knus (which IS appropriate here — registry KDL is Pattern-controlled, strict-unknowns is correct). Manifests themselves live in plugin directories and are read fresh on each daemon start. Three plugin scopes — `Project` (with split shared/private registry files), `Global` (`~/.pattern/plugins/<id>/`), `Ambient` (auto-discovered on-disk plugins not pinned in any registry file). Project > Global > Ambient resolution at discovery time.

**Tech Stack:** `knus` 3.3 (KDL decode, persona-loader pattern), `kdl` 6 (raw doc handling where needed), `serde_json` (CC JSON path), `dirs` (already via `pattern_memory::PatternPaths`), `pattern_memory::jj::JjAdapter` (extended with a `clone` method).

**Scope:** 1 of 7 phases. Order C: registry → hooks → plugin trait+CC shell → CC completion → MCP inverted → IRPC+SDK+McpAdapter → atproto+smoke+cleanup.

**Codebase verified:** 2026-04-27.

---

## Codebase verification findings

- ✓ `crates/pattern_runtime/src/plugin/` does not exist. No leftover stub from earlier phases.
- ✓ Persona-loader at `crates/pattern_runtime/src/persona_loader.rs:217` is the canonical knus-Decode model. Use the same DTO style (`#[derive(Decode)]`, `#[knus(child)]`, `#[knus(children)]`) for `PluginManifest`.
- ✓ `knus 3.3` and `kdl 6` are workspace deps. Persona loader uses knus and rejects unknown KDL fields (`persona_loader.rs:269`) — appropriate there because persona shape is Pattern-controlled and strict. For the plugin manifest we want forward-compat preservation, so the manifest parser drops down to `kdl::KdlDocument` directly and walks nodes manually. Knus stays in use for the registry KDL files (Tasks 6-8), where strict shape is correct.
- ✓ `crates/pattern_memory/src/paths.rs:80` (`PatternPaths`) is the central path resolver. Uses `$PATTERN_HOME` env-var override or `dirs::home_dir().join(".pattern")`. Has `with_base(...)` test-only constructor for fixture isolation.
- ✓ `CapabilitySet` at `crates/pattern_core/src/capability.rs:178-202` derives `Serialize`+`Deserialize`. Persona KDL already decodes it via `CapabilitiesSection` at `persona_loader.rs:377-402`. `PluginManifest` reuses the same KDL block shape for `declared_effects`.
- ✓ Existing precedence pattern at `crates/pattern_memory/src/persona/discover.rs:20-45` (HashMap insert overwrite, project > global). Plugin discovery follows the same shape, extended with a third Ambient tier.
- ✓ ID convention at `crates/pattern_core/src/types/ids.rs:29-87` is `type PersonaId = SmolStr;` — type alias, not newtype. `type PluginId = SmolStr;` matches.
- ✓ `JjAdapter` exists at `crates/pattern_memory/src/jj/adapter.rs:54`. Has `init_repo`, `workspace_add`, `bookmark_set`, `commit`, etc. Needs a `clone(url, dest)` method added (one new method, ~15 LOC).
- ✓ `pattern_runtime/Cargo.toml` already has all required deps: `knus`, `kdl`, `dirs` (transitively via `pattern-memory`), `serde`, `serde_json`, `thiserror`, `miette`, `tracing`, `parking_lot`, `dashmap`, `smol_str`.
- ✓ `pattern_memory/Cargo.toml` is where the JjAdapter `clone` method lands. `pattern-memory` is already a dep of `pattern_runtime`.
- ✓ Project CLAUDE.md updated 2026-04-27 to clarify module convention: `<name>.rs` adjacent to `<name>/` directory, root file re-exports only, logic in submodules.

**External research findings (CC plugin schema, Apr 2026):**
- ✓ CC manifest path: `.claude-plugin/plugin.json` (not at plugin root). Required field: `name` only.
- ✓ Component fields (`skills`, `commands`, `agents`, `hooks`, `mcpServers`, `lspServers`, `monitors`, `outputStyles`, `themes`) accept `string | array | object` — translator must handle all three.
- ✓ `userConfig` object supports `sensitive` flag (keychain hint).
- ✓ Component subdirectories live at plugin root (`./skills/`, `./agents/`, etc.), not under `.claude-plugin/`.
- ✓ Translation table:
  - **Direct map** (CC name → Pattern name): `name`, `version`, `description`, `author`, `homepage`, `repository`, `license`, `keywords`, `skills`, `commands`, `agents`, `hooks`, `mcpServers`→`mcp_servers`, `monitors`, `bin`, `dependencies`.
  - **Preserved under `cc {}`**: `lspServers`, `outputStyles`, `themes`, `channels`, `userConfig`, plus any fields unknown to Pattern's translator.
  - **Pattern-native top-level (CC plugins do not declare these)**: `transport`, `declared_effects`, `pattern { persona_mode }`.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-extensibility.AC1: Plugin manifest parsing

- **v3-extensibility.AC1.1 Success:** KDL-format plugin manifest parses to `PluginManifest` with all declared fields (name, skills, agents, commands, hooks, transport, declared_effects)
- **v3-extensibility.AC1.2 Success:** CC-format JSON `plugin.json` parses to the same `PluginManifest` type; normalized representation matches equivalent KDL manifest
- **v3-extensibility.AC1.3 Success:** Unknown fields in both KDL and JSON manifests are silently ignored; parsing succeeds
- **v3-extensibility.AC1.4 Failure:** Manifest missing required `name` field produces `ManifestError::MissingField("name")` with file path
- **v3-extensibility.AC1.5 Edge:** Manifest with only `name` and no components parses successfully (empty plugin, valid for testing/scaffolding)

### v3-extensibility.AC2: Plugin registry and lifecycle

- **v3-extensibility.AC2.1 Success:** Plugin install clones to `~/.pattern/plugins/cache/<plugin-id>/`; registry records the installation; persisted KDL config written
- **v3-extensibility.AC2.2 Success:** After runtime restart, registry loads from persisted KDL; all previously installed plugins re-registered with their config tunables
- **v3-extensibility.AC2.3 Success:** Plugin uninstall removes from registry and cache; `plugin.uninstall` hook event fires *(NOTE: the actual hook event firing requires the `HookBus` from Phase 2. Phase 1 emits the registry mutation and provides a `HookEmitter` callback seam (`Box<dyn Fn(HookEvent)>`) the registry calls; Phase 2 wires the real bus into that seam. Until then the seam defaults to a no-op closure.)*
- **v3-extensibility.AC2.4 Success:** Load precedence: project-scoped plugin overrides global plugin with same ID; warning logged about the override
- **v3-extensibility.AC2.5 Failure:** Installing a plugin with a collision (same ID at same scope) produces `RegistryError::Collision` with both locations
- **v3-extensibility.AC2.6 Edge:** Plugin config tunables editable in persisted KDL between restarts; changes take effect on next load

---

## Tasks

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: Plugin module scaffolding and core types

**Verifies:** None (infrastructure).

**Files:**
- Create: `crates/pattern_runtime/src/plugin.rs` (module root: re-exports + submodule declarations only).
- Create: `crates/pattern_runtime/src/plugin/error.rs`.
- Create: `crates/pattern_runtime/src/plugin/scope.rs`.
- Modify: `crates/pattern_runtime/src/lib.rs` — add `pub mod plugin;` declaration.

**Implementation:**

In `plugin.rs` declare `pub mod manifest; pub mod registry; pub mod scope; pub mod error;` and re-export the public surface:

```rust
//! Plugin subsystem: manifest parsing, registry, install/uninstall lifecycle.

pub mod error;
pub mod manifest;
pub mod registry;
pub mod scope;

pub use error::{ManifestError, PluginError, RegistryError};
pub use manifest::{Cc, ComponentSpec, PluginManifest};
pub use registry::{LoadedPlugin, PluginInstallation, PluginRegistry};
pub use scope::PluginScope;

/// Stable plugin identifier (kebab-case, matches CC `name` field shape).
pub type PluginId = smol_str::SmolStr;
```

In `error.rs`, define three error enums via `thiserror`, each `#[non_exhaustive]`. Display messages are lowercase sentence fragments per project convention. Errors carry `PathBuf` context for file-related failures so `miette` reports point at the offending file.

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("missing required field {field:?} in manifest at {path}")]
    MissingField { field: &'static str, path: std::path::PathBuf },

    #[error("failed to read manifest at {path}: {source}")]
    Io { path: std::path::PathBuf, #[source] source: std::io::Error },

    #[error("failed to parse KDL manifest at {path}: {message}")]
    Kdl { path: std::path::PathBuf, message: String },

    #[error("failed to parse CC JSON manifest at {path}: {source}")]
    Json { path: std::path::PathBuf, #[source] source: serde_json::Error },
}
```

`RegistryError` covers collision, IO on the registry KDL files, missing-cache-dir, and uninstall-of-unknown-plugin. `PluginError` is the umbrella that registry/manifest errors flatten into for higher layers.

In `scope.rs`, define:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum PluginScope {
    /// Pinned in <project>/.pattern/{shared,private}/plugins.kdl.
    Project { private: bool },
    /// Pinned in ~/.pattern/plugins/registry.kdl.
    Global,
    /// On-disk in ~/.pattern/plugins/<id>/ but not pinned in any registry.
    Ambient,
}
```

`PartialOrd`/`Ord` are derived so precedence comparisons (`Project { .. } > Global > Ambient`) work via comparison operators. The `private: bool` discriminator is necessary because some Project plugins live in `private/` (gitignored) — but for precedence both Project variants are equivalent (private is not "higher" than shared, just stored separately). Document this in a doc-comment on the enum.

**Verification:**
Run: `cargo check -p pattern-runtime`
Expected: builds cleanly, no warnings about unused declarations.

**Commit:** `[pattern-runtime] scaffold plugin module + error/scope types`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Plugin path helpers on `PatternPaths`

**Verifies:** None (infrastructure; consumed by registry tasks).

**Files:**
- Modify: `crates/pattern_memory/src/paths.rs` — add four method helpers on `PatternPaths`.

**Implementation:**

Add four methods next to the existing helpers (e.g., adjacent to `base()` at `paths.rs:~80`):

```rust
impl PatternPaths {
    /// Global plugin install root: `<base>/plugins`.
    pub fn plugins_global_root(&self) -> std::path::PathBuf {
        self.base().join("plugins")
    }

    /// Plugin cache root: `<base>/plugins/cache/`.
    pub fn plugins_cache_root(&self) -> std::path::PathBuf {
        self.plugins_global_root().join("cache")
    }

    /// Per-plugin cache directory: `<base>/plugins/cache/<id>/`.
    pub fn plugin_cache_dir(&self, id: &str) -> std::path::PathBuf {
        self.plugins_cache_root().join(id)
    }

    /// Global registry file: `<base>/plugins/registry.kdl`.
    pub fn plugins_global_registry(&self) -> std::path::PathBuf {
        self.plugins_global_root().join("registry.kdl")
    }
}
```

Project-scoped registry files (`<mount>/.pattern/{shared,private}/plugins.kdl`) are computed by registry code from a `MountInfo` rather than `PatternPaths`, since `PatternPaths` is global-state-only by design. Add a free function to `pattern_memory::paths`:

```rust
pub fn project_plugin_registry(mount_path: &std::path::Path, private: bool) -> std::path::PathBuf {
    let leaf = if private { "private" } else { "shared" };
    mount_path.join(".pattern").join(leaf).join("plugins.kdl")
}
```

**Verification:**
Run: `cargo check -p pattern-memory`
Expected: builds cleanly.

Add a small unit test inside `paths.rs` that exercises the helpers under `PatternPaths::with_base("/tmp/test")` and asserts the returned paths string-match the expected layout.

**Commit:** `[pattern-memory] add plugin path helpers to PatternPaths`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->

<!-- START_TASK_3 -->
### Task 3: `PluginManifest` + KDL decoder

**Verifies:** AC1.1, AC1.4, AC1.5.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/manifest.rs` (manifest type + KDL decoder; CC JSON decoder lands in Task 4).

**Implementation:**

Define the unified `PluginManifest`. **Do NOT derive `knus::Decode`.** Knus is strict-by-design (rejects unknowns at decode time) and lacks a flatten/preserve escape hatch. For a forward-compat plugin manifest we want known fields decoded into typed slots AND unknown nodes preserved verbatim. Use the `kdl` crate (workspace dep, version 6) directly: parse to `KdlDocument`, walk top-level nodes, dispatch known names to typed extractors, collect unknowns into a `BTreeMap<String, KdlNode>` field.

```rust
use kdl::{KdlDocument, KdlNode};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::collections::BTreeMap;
use std::path::PathBuf;

use pattern_core::capability::CapabilitySet;

use super::error::ManifestError;

/// Pattern-native plugin manifest. Parses Pattern KDL via the `kdl` crate
/// directly (preserving unknowns); CC `plugin.json` via the translator in
/// `manifest::cc` (preserving unknowns under the `cc` block).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PluginManifest {
    pub name: SmolStr,
    pub version: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub author: Option<Author>,
    pub keywords: Vec<String>,

    // Component path declarations.
    pub skills: Vec<ComponentSpec>,
    pub commands: Vec<ComponentSpec>,
    pub agents: Vec<ComponentSpec>,
    pub hooks: Vec<ComponentSpec>,
    pub mcp_servers: Vec<ComponentSpec>,
    pub monitors: Vec<ComponentSpec>,
    pub bin: Vec<ComponentSpec>,
    pub dependencies: Vec<DependencySpec>,

    // Pattern-native fields (CC plugins do not declare these).
    pub transport: Option<TransportPreference>,
    pub declared_effects: Option<CapabilitiesBlock>, // reuses persona-loader's shape
    pub pattern: Option<PatternBlock>,

    // CC-specific or unknown-to-Pattern fields preserved verbatim (CC JSON path only).
    pub cc: Option<Cc>,

    /// Unknown top-level KDL nodes preserved verbatim from the Pattern KDL path.
    /// Empty when the manifest came from CC JSON. Forward-compat — Phase 3+ may
    /// lift specific node names out of here as new known fields are added.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_kdl: BTreeMap<String, KdlNode>,
}

/// Residue from CC `plugin.json` parsing — fields the translator did not map
/// to a Pattern equivalent, plus any unknown future fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Cc {
    pub source_format: SmolStr,                            // currently always "plugin.json"
    pub fields: BTreeMap<String, serde_json::Value>,       // structured map, NOT a serialized string
}
```

Define `Author`, `ComponentSpec`, `DependencySpec`, `TransportPreference`, `PatternBlock`, `CapabilitiesBlock` as plain Rust types with `Serialize + Deserialize`. (No `Decode` derive — they're constructed by the manual KDL walker, not by knus.)

`ComponentSpec` enum:
- `Path(PathBuf)` — single path argument
- `Paths(Vec<PathBuf>)` — list
- `Inline(serde_json::Value)` — for hooks/mcpServers configured inline (CC supports `hooks` as either path or inline JSON object). For Pattern KDL, `Inline` accepts a `(json)` arg-typed string holding the JSON literal — the walker decodes the string with `serde_json::from_str`.

Public entry point in `manifest.rs`:

```rust
impl PluginManifest {
    pub fn from_kdl_file(path: &std::path::Path) -> Result<Self, ManifestError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|source| ManifestError::Io { path: path.to_path_buf(), source })?;
        let doc: KdlDocument = raw.parse()
            .map_err(|err: kdl::KdlError| ManifestError::Kdl { path: path.to_path_buf(), message: err.to_string() })?;
        Self::from_kdl_document(&doc, path)
    }

    fn from_kdl_document(doc: &KdlDocument, path: &std::path::Path) -> Result<Self, ManifestError> {
        let mut name: Option<SmolStr> = None;
        let mut version = None;
        // ... per-field locals ...
        let mut unknown_kdl: BTreeMap<String, KdlNode> = BTreeMap::new();

        for node in doc.nodes() {
            match node.name().value() {
                "name"       => name = Some(extract_smol_str_argument(node)?),
                "version"    => version = Some(extract_string_argument(node)?),
                "description"=> description = Some(extract_string_argument(node)?),
                // ... known-field dispatch ...
                "skills"     => skills = parse_component_specs(node)?,
                "hooks"      => hooks = parse_component_specs(node)?,
                "transport"  => transport = Some(parse_transport(node)?),
                "declared_effects" | "declared-effects"
                             => declared_effects = Some(parse_capabilities_block(node)?),
                "pattern"    => pattern = Some(parse_pattern_block(node)?),
                // ... rest of known names ...

                // Unknown — preserve verbatim for forward-compat.
                other => {
                    unknown_kdl.insert(other.to_string(), node.clone());
                }
            }
        }

        let name = name.ok_or_else(|| ManifestError::MissingField { field: "name", path: path.to_path_buf() })?;

        Ok(PluginManifest {
            name, version, description, /* ... */, cc: None, unknown_kdl,
        })
    }
}
```

`extract_*_argument` and `parse_*` helpers live as `pub(crate) fn` siblings in `manifest.rs`. They produce `ManifestError::Kdl { path, message }` on shape errors with a useful diagnostic (offending node name + line/column from `KdlNode::span()`).

**Testing:**

Tests must verify each AC listed above. Task-implementor generates test code at execution time:
- AC1.1: KDL fixture with every field populated parses without error; field-by-field assertions match expected values.
- AC1.4: KDL manifest missing the `name` child returns `ManifestError::MissingField { field: "name", .. }` (note: knus's missing-required-child error needs to be translated into our typed error variant — task-implementor handles the translation logic).
- AC1.5: KDL manifest with only `name "test-plugin"` parses; all `Vec<...>` fields are empty; `cc` is `None`.

Test fixtures live at `crates/pattern_runtime/tests/fixtures/plugins/manifest_full.kdl`, `manifest_missing_name.kdl`, `manifest_minimal.kdl`. Inline tests run in `manifest.rs` itself for fast iteration.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::manifest`
Expected: all unit tests pass.

**Commit:** `[pattern-runtime] add PluginManifest + KDL decoder`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: CC `plugin.json` decoder + translation

**Verifies:** AC1.2, AC1.3.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/manifest/cc.rs` (translator).
- Modify: `crates/pattern_runtime/src/plugin/manifest.rs` — declare `mod cc;` and re-export `cc::translate_cc_json`.

**Implementation:**

Define a CC DTO in `cc.rs` that mirrors the documented `plugin.json` schema. Decode via `serde_json` (which silently ignores unknown fields by default — that gives AC1.3 for the JSON path).

```rust
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

use super::PluginManifest;
use crate::plugin::error::ManifestError;

/// Closely mirrors CC plugin.json. Component fields use `Value` because
/// CC accepts string | array | object — handled by the translator.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CcPluginJson {
    name: smol_str::SmolStr,
    #[serde(default)] version: Option<String>,
    #[serde(default)] description: Option<String>,
    #[serde(default)] author: Option<Value>,
    #[serde(default)] homepage: Option<String>,
    #[serde(default)] repository: Option<String>,
    #[serde(default)] license: Option<String>,
    #[serde(default)] keywords: Vec<String>,

    #[serde(default)] skills: Option<Value>,
    #[serde(default)] commands: Option<Value>,
    #[serde(default)] agents: Option<Value>,
    #[serde(default)] hooks: Option<Value>,
    #[serde(default, rename = "mcpServers")] mcp_servers: Option<Value>,
    #[serde(default)] monitors: Option<Value>,
    #[serde(default)] bin: Option<Value>,
    #[serde(default)] dependencies: Option<Value>,

    // Preserved verbatim under cc {}:
    #[serde(default, rename = "lspServers")] lsp_servers: Option<Value>,
    #[serde(default, rename = "outputStyles")] output_styles: Option<Value>,
    #[serde(default)] themes: Option<Value>,
    #[serde(default)] channels: Option<Value>,
    #[serde(default, rename = "userConfig")] user_config: Option<Value>,

    // Capture truly-unknown fields (forward-compat). serde_json supports this via #[serde(flatten)] + HashMap<String, Value>.
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

pub fn translate_cc_json(path: &std::path::Path) -> Result<PluginManifest, ManifestError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|source| ManifestError::Io { path: path.to_path_buf(), source })?;
    let dto: CcPluginJson = serde_json::from_str(&raw)
        .map_err(|source| ManifestError::Json { path: path.to_path_buf(), source })?;
    Ok(translate(dto))
}

fn translate(dto: CcPluginJson) -> PluginManifest {
    // 1. Coerce component fields (skills/commands/agents/hooks/mcpServers/monitors/bin/dependencies)
    //    into `Vec<ComponentSpec>` via `coerce_component_field`.
    // 2. Build `cc.fields: BTreeMap<String, serde_json::Value>` from:
    //      - explicitly-preserved fields (`lspServers`, `outputStyles`, `themes`, `channels`, `userConfig`)
    //        when their Option is Some — keyed by their CC name.
    //      - captured `extra: BTreeMap<String, Value>` (truly-unknown forward-compat fields).
    //    The BTreeMap is structured JSON — DO NOT serialize to a string blob.
    // 3. Construct PluginManifest with cc = Some(Cc { source_format: "plugin.json".into(), fields }).
    //    transport, declared_effects, pattern, unknown_kdl all None/empty (CC plugins don't declare these).
}
```

The `translate` function is the workhorse:
- Component-field helper `coerce_component_field(value: Option<Value>) -> Vec<ComponentSpec>` handles `string | array-of-strings | array-of-objects | single-object` shapes and produces `Vec<ComponentSpec>`. Malformed shapes coerce into `ComponentSpec::Inline(value)` to preserve forward-compat (CC's path is permissive per AC1.3).
- `Cc.fields` is a structured `BTreeMap<String, serde_json::Value>` — Phase 3+ consumes typed JSON directly via `manifest.cc.as_ref().and_then(|c| c.fields.get("userConfig"))`. NO string re-parse.

**Testing:**

Tests must verify each AC listed above:
- AC1.2: A representative CC `plugin.json` (skills as path string + hooks as inline object + mcpServers as object) translates into a `PluginManifest` that semantically matches a hand-authored Pattern KDL of the same plugin. Verify by deep equality on `PluginManifest` (excluding `cc` for the KDL-equivalent comparison; assert `cc` is `None` for KDL and `Some` for JSON, with `fields` containing only the preserved-verbatim keys).
- AC1.3 (CC JSON path): a `plugin.json` with several unknown future fields (`fooBar: "x"`, `baz: { y: 1 }`) parses successfully; assert `manifest.cc.unwrap().fields.get("fooBar") == Some(&json!("x"))` and `fields.get("baz")` is the structured object. Round-trip preservation is the contract — these are the user-clarified semantics for CC plugins.
- AC1.3 (Pattern KDL path): a manifest with unknown top-level KDL nodes (e.g., `experimental_thing { ... }`) parses successfully; assert `manifest.unknown_kdl.contains_key("experimental_thing")` and the preserved `KdlNode` round-trips when re-serialized. Pattern KDL is also forward-compat — unknowns preserved, not rejected.
- Both formats: Document in the test module that "silently ignored" from the AC text is interpreted as "preserved structurally for forward-compat" — the user-clarified design intent (residue under `cc {}` for CC, `unknown_kdl` for Pattern KDL).

Test fixtures: `tests/fixtures/plugins/cc_full.json`, `cc_unknown_fields.json`, `cc_minimal.json`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::manifest::cc`
Expected: all unit tests pass.

**Commit:** `[pattern-runtime] add CC plugin.json decoder + Pattern-translation`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Manifest tests (integration suite)

**Verifies:** AC1.1, AC1.2, AC1.3, AC1.4, AC1.5 — end-to-end on real fixtures.

**Files:**
- Create: `crates/pattern_runtime/tests/plugin_manifest.rs` (integration suite).
- Create: `crates/pattern_runtime/tests/fixtures/plugins/` directory with the fixtures referenced in Tasks 3 and 4.

**Implementation:**

Integration tests exercise both KDL and CC JSON paths against real on-disk fixtures, asserting `PluginManifest` structural equivalence between matched-pair fixtures.

```rust
#[test]
fn kdl_and_cc_yield_equivalent_manifest() {
    let kdl = PluginManifest::from_kdl_file("tests/fixtures/plugins/equivalent.kdl").unwrap();
    let cc  = translate_cc_json(Path::new("tests/fixtures/plugins/equivalent.json")).unwrap();
    assert_eq!(kdl.name, cc.name);
    assert_eq!(kdl.skills, cc.skills);
    // ... etc; cc.cc may be Some, kdl.cc is None
}
```

**Testing:**
Tests must verify each AC listed above. The fixture-pair test is the most important: it pins the translation contract.

**Verification:**
Run: `cargo nextest run -p pattern-runtime --test plugin_manifest`
Expected: all integration tests pass.

**Commit:** `[pattern-runtime] add plugin manifest integration suite`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-8) -->

<!-- START_TASK_6 -->
### Task 6: `PluginRegistry` types and KDL persistence

**Verifies:** AC2.6 (config tunables editable in persisted KDL).

**Files:**
- Create: `crates/pattern_runtime/src/plugin/registry.rs`.

**Implementation:**

`PluginRegistry` is a per-mount-aware registry. Construction takes a `PatternPaths` (global) and an optional mount root (project-scoped registries). Internal storage:

```rust
use parking_lot::RwLock;
use smol_str::SmolStr;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pattern_core::capability::CapabilitySet;
use pattern_memory::paths::{project_plugin_registry, PatternPaths};

use super::manifest::PluginManifest;
use super::scope::PluginScope;
use super::error::RegistryError;
use super::PluginId;

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub id: PluginId,
    pub scope: PluginScope,
    pub source_path: PathBuf,           // Where the plugin lives on disk (cache or project dir).
    pub manifest: PluginManifest,
    pub user_config: serde_json::Value, // From userConfig declarations + KDL overrides.
    pub capability_overrides: Option<CapabilitySet>, // User-pinned capability narrowing/broadening.
}

#[derive(Debug, Clone, knus::Decode)]
pub struct PluginInstallation {
    #[knus(argument)]
    pub id: SmolStr,
    #[knus(child, unwrap(argument), default)]
    pub source: Option<String>,         // Local path or git URL captured at install time.
    #[knus(child, unwrap(argument), default)]
    pub installed_at: Option<String>,   // ISO 8601, jiff::Timestamp formatted.
    #[knus(child, default)]
    pub user_config: Option<UserConfigBlock>,    // KDL representation of user-tunable values.
    #[knus(child, default)]
    pub capability_override: Option<CapabilitiesBlock>,
}

#[derive(Debug, Clone, knus::Decode)]
pub struct RegistryFile {
    #[knus(children(name = "plugin"))]
    pub plugins: Vec<PluginInstallation>,
}

pub struct PluginRegistry {
    paths: Arc<PatternPaths>,
    mount_path: Option<PathBuf>,
    inner: RwLock<HashMap<PluginId, LoadedPlugin>>,
    hook_emit: HookEmitter, // Phase 1 default: no-op closure; Phase 2 wires real bus.
}

pub type HookEmitter = Box<dyn Fn(/* tag */ &str, /* payload */ serde_json::Value) + Send + Sync>;
```

`UserConfigBlock` is a freeform map decoded from KDL — `Vec<(String, KdlValue)>` materialized as a `serde_json::Value::Object` for runtime use. Define it inline in `registry.rs`.

The `HookEmitter` callback seam is the Phase 2 integration point referenced by AC2.3. `PluginRegistry::with_hook_emitter(emitter)` swaps the no-op default with a real emitter when the bus is available.

Persistence shape (KDL):

```kdl
// ~/.pattern/plugins/registry.kdl
plugin "code-quality" {
    source "/home/user/.local/plugins/code-quality"
    installed-at "2026-04-27T14:32:00Z"
    user-config {
        api-token (sensitive)"keychain:code-quality.token"
        threshold 8
    }
}

plugin "format-on-save" {
    source "https://github.com/example/format-on-save"
    installed-at "2026-04-27T15:00:00Z"
}
```

Add I/O methods:

```rust
impl PluginRegistry {
    pub fn load(paths: Arc<PatternPaths>, mount_path: Option<PathBuf>) -> Result<Self, RegistryError> { /* see Task 7 */ }
    pub fn save(&self) -> Result<(), RegistryError> { /* writes to all relevant scopes */ }
    pub fn list(&self) -> Vec<LoadedPlugin> { /* clone snapshot under read lock */ }
    pub fn get(&self, id: &str) -> Option<LoadedPlugin> { /* clone */ }
}
```

Saving writes only the registry files for scopes that have changed (`Project { private: true }`, `Project { private: false }`, `Global`). Ambient plugins are not persisted.

**Testing:**
Tests must verify each AC listed above:
- AC2.6: Edit `user_config.threshold` in a fixture registry KDL file, call `PluginRegistry::load(...)` again, assert the new value is reflected on the corresponding `LoadedPlugin`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::registry`
Expected: type-construction and persistence-shape tests pass. Discovery + install/uninstall tests are added in Task 8.

**Commit:** `[pattern-runtime] add PluginRegistry types + KDL persistence`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Discovery, install (local-path + jj git clone), uninstall

**Verifies:** AC2.1, AC2.2, AC2.3, AC2.4, AC2.5.

**Files:**
- Modify: `crates/pattern_runtime/src/plugin/registry.rs` — add `discover`, `install`, `uninstall` methods.
- Modify: `crates/pattern_memory/src/jj/adapter.rs` — add `clone(url, dest)` method.

**Implementation:**

Add `clone` to `JjAdapter`:

```rust
impl JjAdapter {
    /// `jj git clone <url> <dest>` — clones a remote repo into a fresh jj-managed checkout.
    /// `dest` must not exist; returns `JjError::DestinationExists` otherwise.
    pub fn clone(&self, url: &str, dest: &Path) -> JjResult<()> {
        // Reject existing destination.
        if dest.exists() {
            return Err(JjError::DestinationExists(dest.to_path_buf()));
        }
        let _guard = self.mutation_lock.lock().unwrap();
        self.cmd("git")
            .arg("clone")
            .arg(url)
            .arg(dest)
            .status()
            .and_then(check_status)
    }
}
```

Add `JjError::DestinationExists(PathBuf)` to the existing error enum.

In `registry.rs`, the discovery logic walks all three scopes and applies precedence:

```rust
impl PluginRegistry {
    /// Build the registry by reading both registry KDL files (if present)
    /// plus auto-discovering on-disk plugins not pinned anywhere.
    pub fn load(paths: Arc<PatternPaths>, mount_path: Option<PathBuf>) -> Result<Self, RegistryError> {
        let mut combined: HashMap<PluginId, LoadedPlugin> = HashMap::new();

        // 1. Ambient (lowest precedence) — every directory under <base>/plugins/ that contains a manifest.
        for entry in scan_plugin_dirs(&paths.plugins_global_root())? {
            let manifest = load_manifest(&entry.path)?;
            let lp = LoadedPlugin {
                id: manifest.name.clone(),
                scope: PluginScope::Ambient,
                source_path: entry.path,
                manifest,
                user_config: serde_json::Value::Null,
                capability_overrides: None,
            };
            combined.insert(lp.id.clone(), lp);
        }

        // 2. Global pins — overwrites any ambient with the same id; warn on override.
        if let Some(file) = read_registry_file(&paths.plugins_global_registry())? {
            for inst in file.plugins {
                let manifest = load_manifest(&paths.plugin_cache_dir(&inst.id))?;
                let lp = build_loaded(inst, manifest, PluginScope::Global, &paths)?;
                if let Some(prev) = combined.insert(lp.id.clone(), lp) {
                    warn_on_override(&prev, PluginScope::Global);
                }
            }
        }

        // 3. Project pins — shared first then private.
        if let Some(mp) = &mount_path {
            for private in [false, true] {
                let path = project_plugin_registry(mp, private);
                if let Some(file) = read_registry_file(&path)? {
                    for inst in file.plugins {
                        let manifest = load_manifest(&inst.source_path_resolved(mp, &paths))?;
                        let lp = build_loaded(inst, manifest, PluginScope::Project { private }, &paths)?;
                        if let Some(prev) = combined.insert(lp.id.clone(), lp) {
                            warn_on_override(&prev, PluginScope::Project { private });
                        }
                    }
                }
            }
        }

        Ok(Self {
            paths,
            mount_path,
            inner: RwLock::new(combined),
            hook_emit: Box::new(|_, _| {}),
        })
    }
}
```

`warn_on_override` calls `tracing::warn!(plugin_id = %prev.id, prev_scope = ?prev.scope, new_scope = ?new_scope, "plugin override: project/global pin shadows lower-precedence registration");` — satisfies AC2.4's "warning logged" requirement.

`install` is parametrized by source kind and target scope:

```rust
pub enum InstallSource<'a> {
    LocalPath(&'a Path),
    JjGitUrl(&'a str),
}

impl PluginRegistry {
    pub fn install(
        &self,
        source: InstallSource<'_>,
        target_scope: PluginScope,
        jj: &pattern_memory::jj::JjAdapter,
    ) -> Result<PluginId, RegistryError> {
        // 1. Stage the plugin source into a temp dir under cache_root/.staging-<random>/.
        // 2. Read manifest from staged dir; that gives us the plugin id.
        // 3. Compute final cache dir = paths.plugin_cache_dir(&manifest.name).
        // 4. Collision check: if final dir exists AND we're installing at the same scope, return RegistryError::Collision.
        //    (Different-scope re-install is *not* a collision; it's a precedence override and proceeds with a warn.)
        // 5. Atomic rename(staging_dir -> final_dir).
        // 6. Append PluginInstallation entry to the appropriate registry KDL file.
        // 7. Insert LoadedPlugin into self.inner.
        // 8. (self.hook_emit)("plugin.install", json!({...}));
    }
}
```

For `LocalPath`, staging copies recursively (`fs_extra::dir::copy` or a hand-rolled walker — prefer hand-rolled to avoid pulling in `fs_extra` for one operation).

For `JjGitUrl`, staging is `jj.clone(url, &staging_dir)`. The successful `jj clone` produces a working checkout at `staging_dir`; rename into the final cache path is the same as the local-path branch.

`uninstall(id, scope)` removes the plugin from the relevant registry KDL file, removes the cache directory if scope was Global (Project plugins live in the project tree, not cache; don't touch them), removes the `LoadedPlugin` from `self.inner`, and emits `plugin.uninstall`.

Collision (AC2.5): `RegistryError::Collision { id, existing_scope, attempted_scope, existing_path, attempted_path }` — field-rich so the error message can identify both locations.

**Testing:**
Tests must verify each AC listed above:
- AC2.1: Install a fixture plugin from a local path; assert the cache directory exists at `~/.pattern/plugins/cache/<id>/` (use `with_base(tempdir)`); assert the registry KDL file contains the new `plugin "<id>" { ... }` entry; assert `registry.get(&id)` returns the plugin with `scope: PluginScope::Global`.
- AC2.2: Install plugin → drop `PluginRegistry` → reconstruct with `PluginRegistry::load(...)` against the same paths → assert all installed plugins re-loaded with their config tunables intact.
- AC2.3: Uninstall a plugin → assert `registry.get(&id)` is `None`, cache dir gone, KDL file no longer contains the entry. The hook seam is asserted with a custom `hook_emit` closure that captures emitted events into a `Vec<_>`.
- AC2.4: Install global; install project at same id; assert `registry.get(&id).scope == Project { .. }` and `tracing-test`'s `traced_test` macro catches a warn-level log line containing both scope names.
- AC2.5: Install plugin "x" globally → install plugin "x" globally again → assert `RegistryError::Collision` with both `existing_path` and `attempted_path` populated.

`tempfile::TempDir` for cache root isolation per test. `tracing-test = "0.2"` is a workspace-acceptable dev-dep for log assertions; if it's not already present, the executor adds it. (Verify in `pattern_runtime/Cargo.toml`.)

The jj-clone path is exercised by a single test that uses `jj.init_repo(...)` to create a tiny ephemeral source repo on disk, then installs from `file://` URL. No network required.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::registry`
Expected: all registry tests pass.

**Commit:** `[pattern-runtime] [pattern-memory] add registry discovery + install/uninstall + JjAdapter::clone`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Registry integration suite + project-scope tests

**Verifies:** AC2.1, AC2.2, AC2.3, AC2.4, AC2.5, AC2.6 — end-to-end against real on-disk paths.

**Files:**
- Create: `crates/pattern_runtime/tests/plugin_registry.rs`.
- Create: `crates/pattern_runtime/tests/fixtures/plugins/cache-source/` with one minimal plugin layout used by multiple tests.

**Implementation:**

Integration tests cover end-to-end install/restart/uninstall and the project-scoped tier specifically (Task 7's unit tests focus on global-scope happy path).

```rust
#[test]
fn project_scope_overrides_global() {
    let base = tempfile::TempDir::new().unwrap();
    let project = tempfile::TempDir::new().unwrap();
    let paths = Arc::new(PatternPaths::with_base(base.path()));
    let jj = pattern_memory::jj::JjAdapter::detect().unwrap().unwrap();

    // Install id=foo at global, then at project.
    let reg = PluginRegistry::load(paths.clone(), Some(project.path().to_path_buf())).unwrap();
    reg.install(InstallSource::LocalPath(SOURCE), PluginScope::Global, &jj).unwrap();
    reg.install(InstallSource::LocalPath(SOURCE), PluginScope::Project { private: false }, &jj).unwrap();

    let lp = reg.get("foo").unwrap();
    assert!(matches!(lp.scope, PluginScope::Project { .. }));
}
```

**Testing:**
- Restart-survival test (AC2.2) explicitly drops the `PluginRegistry` and rebuilds.
- Tunable-edit test (AC2.6) writes a fixture KDL with `threshold 8`, loads, asserts; then writes the same file with `threshold 12`, reloads, asserts the change appears.
- Private-vs-shared scope test ensures plugins installed to `Project { private: true }` land in the gitignored file, plugins installed to `Project { private: false }` land in the committed file.

**Verification:**
Run: `cargo nextest run -p pattern-runtime --test plugin_registry`
Expected: all integration tests pass; full Phase 1 test surface green.

**Commit:** `[pattern-runtime] add plugin registry integration suite`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase done-when checklist

- [ ] `crates/pattern_runtime/src/plugin.rs` exists; module compiles.
- [ ] `PluginManifest` decodes Pattern KDL manifests with all declared fields.
- [ ] `translate_cc_json(...)` decodes CC `plugin.json` into `PluginManifest`; preserved-verbatim and unknown fields land in `manifest.cc.fields` (structured `BTreeMap<String, serde_json::Value>`, not a serialized string).
- [ ] `JjAdapter::clone(url, dest)` lands in `pattern_memory::jj::adapter`; `JjError::DestinationExists` added.
- [ ] `PluginRegistry::load` resolves project > global > ambient precedence and emits warn-level overrides.
- [ ] `PluginRegistry::install` works for both local-path and jj-git-url sources; collision detected.
- [ ] `PluginRegistry::uninstall` removes from registry KDL + cache; `plugin.uninstall` emitted via the `HookEmitter` seam (no-op default in Phase 1).
- [ ] All Phase 1 tests pass under `cargo nextest run -p pattern-runtime plugin`.
- [ ] `cargo fmt` + `cargo clippy --all-features --all-targets` clean.

---

## Notes for executor

- Do NOT wire the `HookEmitter` to a real bus in this phase. That's Phase 2's job. The default no-op closure is correct here.
- `PluginManifest.cc.fields` is opaque-to-Phase-1 (typed JSON, but Phase 1 doesn't interpret keys). Phase 3 (CC adapter shell) starts consuming specific keys. Do not introduce code that interprets CC residue here — keep the boundary clean.
- The `tracing-test` dev-dep is the recommended way to assert log lines for AC2.4. Confirm whether it's already in `Cargo.toml`; if not, add it as `[dev-dependencies] tracing-test = "0.2"` (workspace dep style).
- For deterministic install timestamps in tests, expose a `PluginRegistry::with_clock(clock_fn)` builder on the registry that defaults to `jiff::Timestamp::now`. Tests pass a fixed `|| Timestamp::from_second(0).unwrap()`.
- Per project guidance: do not silently skip implicit work. If the executor finds, e.g., that `JjAdapter::clone` requires additional plumbing in `JjError` or that the persona-loader's `format_knus_error` helper isn't yet `pub(crate)`, fix those gaps in this phase rather than working around them.
- The "plugin install fires `plugin.install` hook event" AC item is sometimes-listed across Phase 1 and Phase 2 implementation plans. Phase 1 builds the seam; Phase 2 connects the bus. Do not stub or comment-out the emit call — the no-op closure is the explicit, working contract.
- Fixtures under `tests/fixtures/plugins/` are reused by Phase 3 and beyond (CC adapter tests). Keep them small, self-contained, well-named.
