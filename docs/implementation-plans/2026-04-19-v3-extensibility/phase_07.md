# v3-extensibility Phase 7: Trust enforcement + atproto plugin auth + smoke test + cleanup

**Goal:** Activate the ad-hoc skill body-redact + user-enable flow (Plan 2 reserved this, no Pattern phase has built it). Add per-plugin capability overrides via `.pattern.kdl`. Land remote plugin auth via opaque atproto records (node-key only, dag-cbor signed) with backlink-driven counterpart discovery, using jacquard for record publish/resolve/verify and keyring for per-plugin keypair storage. End-to-end smoke test at `tests/plugin_smoke.rs` exercising CC adapter, IRPC native plugin, McpPluginAdapter, hook events, MCP inverted surface, port registration, capability enforcement. Audit + cleanup: delete `crates/pattern_mcp/` from disk, update project CLAUDE.md.

**Architecture:**

**Ad-hoc skills.** `SkillMetadata` gains `enabled: bool` field (defaults to `false` for `SkillTrustTier::AdHoc` skills, `true` for everything else). New `pattern_db` table `skill_approvals` records per-`(skill_label, partner_id)` user-approval state. Skills handler's `Load` checks the flag at dispatch — if `enabled == false` and tier is `AdHoc`, the body is replaced with a redaction marker (`"[skill body redacted; approval required: /skill-enable <label>"`). New CLI subcommand `pattern skills enable <label>` (and equivalent slash command `/skill-enable`) flips the flag and persists to the approvals table.

**Per-plugin capability overrides.** `.pattern.kdl` (project) and `~/.pattern/config.kdl` (global) gain a top-level `plugin_overrides {}` block. Each entry narrows or expands a plugin's manifest-declared capabilities:

```kdl
plugin_overrides {
    plugin "github-bridge" {
        capabilities { effects { mcp; message } }    // narrows to subset
    }
    plugin "trusted-internal" {
        capabilities { effects { ... } flags { spawn-new-identities } }  // user expanded
    }
}
```

At `PluginExtension::on_enable`, the runtime computes the effective `CapabilitySet` as the intersection of (manifest-declared, user-override) — if user override is present, intersection with that; otherwise just manifest. Empty intersection → plugin enabled with `CapabilitySet::default()` (pure computation only — no effects allowed; AC7.6).

**Atproto remote plugin auth.** When a plugin manifest declares `transport.remote { atproto-auth }`, install + enable layer atproto record exchange on top of Phase 6's iroh node-identity gate. Records published to PDS are intentionally opaque:

```rust
// at://<did>/systems.atproto.plugin.session/<rkey> body, dag-cbor canonical:
{
    "$type": "systems.atproto.plugin.session",
    "nodeUri": "node:01a3f4...",        // iroh node ID, formatted as uri-shaped string for indexing
    "createdAt": "2026-04-27T15:00:00Z",
    "sig": "<base64 ed25519 sig over the canonical dag-cbor of {nodeUri, createdAt}>"
}
```

No DID, no plugin id, no purpose information in the record body. The counterpart's DID is discovered out-of-band (local config file pinning the plugin's DID at install time) OR via Constellation backlink query ("who else published a record where the `nodeUri` field equals this value?"). The lexicon is intentionally minimal so future migration to permissioned data adds only a `permission` flag, no structural changes.

**Backlink discovery via Constellation.** Constellation is a public hosted XRPC service at `https://constellation.microcosm.blue` (operated by the atproto community). It indexes inbound field-level references across atproto records observed via firehose — no Pattern-side indexer needed. Pattern queries it anonymously via the `blue.microcosm.links.getBacklinks` XRPC method:

```
GET /xrpc/blue.microcosm.links.getBacklinks
  ?subject=node:01a3f4...
  &source=systems.atproto.plugin.session:nodeUri
  &limit=100
```

Returns `{ total, records: [{ did, collection, rkey }, ...], cursor }`. Pattern then hydrates each candidate via `jacquard.get_record(at://<did>/<collection>/<rkey>)` and verifies signatures — first match wins. No SQLite table, no PDS firehose subscription, no pattern-side caching of the index.

**Per-plugin keypair management.** At plugin install (`PluginRegistry::install`), if manifest declares atproto auth, the runtime:
1. Generates an Ed25519 keypair for the plugin (via `ed25519-dalek`, already a transitive dep of jacquard).
2. Stores the private half in keyring under `pattern.plugin.<plugin-id>.atproto`.
3. Publishes a session record on the user's PDS via jacquard targeting the plugin's eventual node ID.
4. Stores the published record's AT URI on the plugin's `LoadedPlugin` for verification at connect time.

At plugin connect (out-of-process, remote), Pattern verifies the inbound iroh connection's pubkey matches the record's `nodeUri` field, fetches the counterpart's record (from a pinned DID or via Constellation backlink discovery), and verifies the signature. Mismatch = rejected. Same flow works for same-user (records in same repo) and cross-user (records in different repos).

**Smoke test.** Single integration test exercises the full surface: install a CC fixture plugin (Phase 3+4), install a native IRPC plugin (Phase 6), wrap an MCP server via `McpPluginAdapter` (Phase 6), trigger turn → tool dispatch → memory write hook events fire, MCP inverted surface system reminder appears, agent invokes a CC plugin command via slash dispatch, verify capability enforcement denies an out-of-scope effect.

**Cleanup.** `crates/pattern_mcp/` directory removed (already out of workspace, only orphan symlinks + stub source). CLAUDE.md project status updated.

**Tech Stack:** `jacquard 0.12+` (already workspace dep), `keyring` (already workspace dep), `ed25519-dalek` (transitive via jacquard or added directly), `serde_ipld_dagcbor` (transitive via jacquard or added directly), existing iroh from Phase 6.

**Scope:** 7 of 7 phases.

**Codebase verified:** 2026-04-27.

---

## Codebase verification findings

- ⚠ **Ad-hoc skill redaction infrastructure is greenfield.** `SkillMetadata` lacks `enabled` field; no approval table; `Load` handler at `crates/pattern_runtime/src/sdk/handlers/skills.rs:435` returns the full body unconditionally. `SkillTrustTier::AdHoc` exists as enum value but has no enforcement. Phase 7 adds the field + approval table + handler check + CLI/RPC enable flow.
- ⚠ **Per-plugin capability overrides KDL grammar is greenfield.** Persona-level `capabilities {}` and `policy {}` parsing exists at `crates/pattern_runtime/src/persona_loader.rs` (canonical pattern). Plugin-scope overrides need a new top-level KDL block in `.pattern.kdl` (project) + global config. No `plugin_overrides {}` parser exists.
- ✓ **Constellation is a public hosted XRPC service**, NOT something Pattern builds. Reference impl in `~/Projects/vodplace/src/catalog.rs:100-122` defines the request shape via `jacquard_derive::XrpcRequest` (NSID `blue.microcosm.links.getBacklinks`). Pattern adds a thin XRPC client wrapper — no indexer, no firehose subscription, no SQLite table. Anonymous queries; no auth. Naming note: Pattern's existing internal "Constellation" is the agent-grouping abstraction (`pattern_core::constellation::ConstellationRegistry` at `crates/pattern_core/src/constellation.rs`); the *atproto Constellation* is a separate concept — code/docs/tests should use a distinct namespace (e.g., `pattern_runtime::atproto::constellation_client` or `microcosm_links_client`) to avoid confusion.
- ⚠ **`jacquard` is declared as workspace dep but unused.** Phase 7 wires it.
- ✓ `keyring` is workspace dep at `Cargo.toml:135-139`. Plugin keypair storage uses it.
- ✓ `crates/pattern_mcp/` is safe to delete: out of workspace, no internal references found via grep, marked "pre-v3 shape" in project CLAUDE.md.
- ✓ Smoke test convention: `tests/<name>_smoke.rs` (existing examples: `task_skill_smoke.rs`, `sandbox_io_smoke.rs`, `multi_agent_smoke.rs`). Phase 7 follows the same shape with `plugin_smoke.rs`.
- ✓ `MockProviderClient::with_turns(...)` from v3-multi-agent Phase 7 is available for scripted-turn injection.
- ✓ `populated_spawn_test_table()` at `crates/pattern_runtime/src/testing.rs` is the test-side DataConTable for spawn handler dispatch — Phase 7 smoke uses it.
- ✓ Project CLAUDE.md `Last verified: 2026-04-26` (line 7); status block (lines 5-6) shows the format for "v3-X (N phases) complete" entries.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-extensibility.AC6: Remote auth (final)

- **v3-extensibility.AC6.6 Success:** Per-plugin cryptographic auth via iroh node identity (local) ✓ Phase 6; **atproto-backed mutual auth (remote)** ← Phase 7

### v3-extensibility.AC7: Trust enforcement (full)

- **v3-extensibility.AC7.1 Success:** Skills from installed plugins receive `trust_tier: PluginInstalled` via the code path reserved in Plan 2 ✓ Phase 3; verified end-to-end here
- **v3-extensibility.AC7.2 Success:** Plugin capabilities scoped per manifest declaration; plugin agent cannot use effects beyond what the manifest declares
- **v3-extensibility.AC7.3 Success:** User override in KDL config can expand or restrict a plugin's declared capabilities; override takes precedence
- **v3-extensibility.AC7.4 Success:** Ad-hoc skill (non-plugin source) triggers body-redact + user-enable flow on first use
- **v3-extensibility.AC7.5 Failure:** Plugin agent attempts to use an effect not in its manifest-declared or user-overridden capabilities; rejected at prelude filtering (compile-time)
- **v3-extensibility.AC7.6 Edge:** Plugin with no declared capabilities gets an empty CapabilitySet; can only perform pure computation

### v3-extensibility.AC8: End-to-end integration

- **v3-extensibility.AC8.1 Success:** Smoke test at `crates/pattern_runtime/tests/plugin_smoke.rs` passes: installs CC-format plugin (via CcPluginAdapter), installs native IRPC plugin, wraps MCP server (via McpPluginAdapter), verifies skill trust tiers, hook events fire, MCP inverted surface works, port registration works, capability enforcement active
- **v3-extensibility.AC8.2 Success:** Mock ProviderClient and mock MCP server (stdio); no live model or network dependency in CI
- **v3-extensibility.AC8.3 Success:** `pattern_mcp` crate fully removed from workspace; all MCP client code lives in `pattern_runtime`
- **v3-extensibility.AC8.4 Failure:** Any step in the smoke flow failing produces a clear error identifying which step and which assertion
- **v3-extensibility.AC8.5 Edge:** Plugin smoke test runs concurrently with other tests without shared-state interference

---

## Tasks (8 total, 4 subcomponents)

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: Ad-hoc skill body-redaction + approval table

**Verifies:** AC7.4 foundation.

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/skill.rs:60-78` — add `enabled: bool` field with `#[serde(default = "default_enabled")]`, default true.
- Modify: `crates/pattern_runtime/src/sdk/handlers/skills.rs:435` — `handle_load` checks `enabled` AND `trust_tier == AdHoc`; if redacted, return marker text instead of body.
- Create: `crates/pattern_db/migrations/<NN>_add_skill_approvals.sql` — new `skill_approvals` table.
- Create: `crates/pattern_db/src/queries/skill_approvals.rs` — `get_approval`, `record_approval`, `revoke_approval`.

**Implementation:**

`SkillMetadata` change:

```rust
pub struct SkillMetadata {
    pub name: String,
    pub trust_tier: SkillTrustTier,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub hooks: serde_json::Value,
    #[serde(default)]
    pub source: Option<SkillSource>,         // from Phase 5
    /// AdHoc skills require explicit user approval. Always true for non-AdHoc tiers.
    /// Defaults to true for new skills (set to false explicitly when materializing AdHoc).
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool { true }
```

For AdHoc skills materialized via `Memory.Put`, the handler explicitly sets `enabled: false` at creation time. Plugin-installed and project-local skills set `enabled: true`. The skills `Load` handler:

```rust
pub fn handle_load(store: &Arc<dyn MemoryStore>, conn: &..., agent_id: &..., handle: &Handle) -> Result<String, SkillHandlerError> {
    let block = store.get_block(handle)?;
    let metadata = block.metadata::<SkillMetadata>()?;

    if !metadata.enabled && metadata.trust_tier == SkillTrustTier::AdHoc {
        return Ok(format!(
            "[skill body redacted: {} requires user approval]\n\nThis skill was installed ad-hoc (not from a plugin or project). To approve and reveal its body, the partner must run:\n  /skill-enable {}\n",
            metadata.name, handle.label
        ));
    }

    // Record load timestamp.
    skill_usage::record_load(conn, agent_id, &handle.label)?;
    Ok(render_skill_loaded_text(&metadata.name, metadata.trust_tier, &block.body))
}
```

`skill_approvals` migration:

```sql
CREATE TABLE skill_approvals (
    skill_label TEXT NOT NULL,
    partner_id TEXT NOT NULL,
    approved_at INTEGER NOT NULL,         -- jiff timestamp microseconds
    revoked_at INTEGER,
    PRIMARY KEY (skill_label, partner_id)
);
```

`pattern_db` queries:

```rust
pub fn get_approval(conn: &Connection, skill_label: &str, partner_id: &str) -> Result<Option<jiff::Timestamp>>;
pub fn record_approval(conn: &Connection, skill_label: &str, partner_id: &str, ts: jiff::Timestamp) -> Result<()>;
pub fn revoke_approval(conn: &Connection, skill_label: &str, partner_id: &str) -> Result<()>;
```

When `record_approval` fires, the corresponding skill block's `metadata.enabled` is updated to `true` and persisted (via `MemoryStore::update_block_metadata`).

**Testing:**
Tests must verify each AC listed above:
- AC7.4 (foundation): Materialize an `AdHoc` skill via `Memory.Put` (`enabled: false` by default). Call `handle_load`; assert response contains `"redacted"` and the slash-command hint.
- After `record_approval(label, partner_id, now)` + metadata update; subsequent `handle_load` returns the real body.
- Revocation: `revoke_approval`; metadata.enabled flipped back to false; subsequent loads redact.
- Plugin-installed skills (`trust_tier: PluginInstalled`) and project-local (`ProjectLocal`) skills load unredacted with `enabled: true` baseline.

**Verification:**
Run: `cargo nextest run -p pattern-runtime sdk::handlers::skills` and `cargo nextest run -p pattern-db queries::skill_approvals`.

**Commit:** `[pattern-core] [pattern-db] [pattern-runtime] add skill body-redaction + approvals table for AdHoc tier`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: User-enable CLI/RPC flow for AdHoc skills

**Verifies:** AC7.4.

**Files:**
- Modify: `crates/pattern_server/src/protocol.rs` — new `PatternProtocol::EnableSkill(EnableSkillRequest)` RPC.
- Modify: `crates/pattern_server/src/server.rs` — handler dispatches to `pattern_db::skill_approvals::record_approval` + `MemoryStore::update_block_metadata`.
- Modify: `crates/pattern_cli/src/main.rs` (or appropriate CLI module) — new `pattern skills enable <label>` subcommand.
- Modify: `crates/pattern_cli/src/commands.rs` (or wherever slash commands are registered post-Phase 4) — register `/skill-enable <label>` slash command via `CommandRegistry`.

**Implementation:**

RPC:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnableSkillRequest {
    pub skill_label: SmolStr,
    pub revoke: bool,                      // false = enable, true = revoke
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnableSkillResponse {
    pub previous_state: SkillEnabledState,
    pub new_state: SkillEnabledState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SkillEnabledState { Enabled, Redacted, NotFound }
```

Server dispatch:

```rust
async fn handle_enable_skill(
    &self,
    req: EnableSkillRequest,
    partner_id: &SmolStr,
    db: &ConstellationDb,
    store: &Arc<dyn MemoryStore>,
) -> Result<EnableSkillResponse> {
    let label = req.skill_label.as_str();
    let block = store.get_block(label).await?;
    let mut metadata: SkillMetadata = serde_json::from_value(block.metadata)?;

    let prev_state = if metadata.enabled { SkillEnabledState::Enabled } else { SkillEnabledState::Redacted };
    metadata.enabled = !req.revoke;

    store.update_block_metadata(label, serde_json::to_value(&metadata)?).await?;
    if req.revoke {
        skill_approvals::revoke_approval(db, label, partner_id).await?;
    } else {
        skill_approvals::record_approval(db, label, partner_id, jiff::Timestamp::now()).await?;
    }

    Ok(EnableSkillResponse {
        previous_state: prev_state,
        new_state: if metadata.enabled { SkillEnabledState::Enabled } else { SkillEnabledState::Redacted },
    })
}
```

CLI:

```bash
pattern skills enable <label>           # approve
pattern skills enable <label> --revoke  # revoke
```

Slash command (registered via Phase 4's `CommandRegistry`):

```rust
struct SkillEnableHandler { client: Arc<DaemonClient> }

#[async_trait]
impl CommandHandler for SkillEnableHandler {
    fn name(&self) -> &str { "skill-enable" }
    fn audience(&self) -> CommandAudience { CommandAudience::Partner }
    async fn handle(&self, args: &[String], _origin: &Author) -> Result<CommandResponse, CommandError> {
        let label = args.first().ok_or_else(|| CommandError::HandlerFailed { ... })?;
        let resp = self.client.enable_skill(label.clone(), false).await?;
        Ok(CommandResponse {
            content: format!("skill `{label}` is now {:?}", resp.new_state),
            kind: CommandResponseKind::SystemMessage,
            side_effects: vec![],
        })
    }
}
```

**Testing:**
Tests must verify each AC listed above:
- AC7.4 (full): End-to-end via `DaemonClient::from_local`. Materialize an AdHoc skill; assert `Load` returns redacted. Call `enable_skill(label, false)`; assert `Load` returns body. Call `enable_skill(label, true)` to revoke; assert `Load` returns redacted again.
- Slash-command path: dispatch `/skill-enable test-skill` via `CommandRegistry`; assert response.kind == SystemMessage; assert metadata flipped.

**Verification:**
Run: `cargo nextest run -p pattern-server enable_skill` and `cargo nextest run -p pattern-cli skills`.

**Commit:** `[pattern-server] [pattern-cli] add skill enable RPC + CLI subcommand + slash command`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (task 3) -->

<!-- START_TASK_3 -->
### Task 3: Per-plugin capability overrides in `.pattern.kdl`

**Verifies:** AC7.2, AC7.3, AC7.5, AC7.6.

**Files:**
- Modify: `crates/pattern_memory/src/config/pattern_kdl.rs` — add `plugin_overrides {}` block parsing.
- Modify: `crates/pattern_runtime/src/plugin/registry.rs` — `LoadedPlugin.effective_capabilities()` computes intersection.
- Modify: `crates/pattern_runtime/src/sdk/preamble.rs::build_for(caps)` — already filters by capability set; verify the path used by plugin-spawned agents calls into this with the effective set.

**Implementation:**

`.pattern.kdl` grammar extension:

```kdl
plugin_overrides {
    plugin "github-bridge" {
        // User narrows the manifest's broader declaration.
        capabilities {
            effects { mcp; message }
        }
    }
    plugin "trusted-internal" {
        // User explicitly grants flag the manifest didn't request.
        capabilities {
            effects { memory; spawn; shell }
            flags { spawn-new-identities }
        }
    }
}
```

Decoded via knus:

```rust
#[derive(Debug, Decode)]
pub struct PluginOverridesSection {
    #[knus(children(name = "plugin"))]
    pub plugins: Vec<PluginOverrideEntry>,
}

#[derive(Debug, Decode)]
pub struct PluginOverrideEntry {
    #[knus(argument)]
    pub plugin_id: SmolStr,
    #[knus(child)]
    pub capabilities: Option<CapabilitiesSection>,   // reuse persona-loader DTO
}
```

Stored on `PatternConfig` (Phase 1 of v3-sandbox-io's config struct). Wired into `ProjectMount` when building the mount.

`LoadedPlugin::effective_capabilities()`:

```rust
impl LoadedPlugin {
    pub fn effective_capabilities(&self, override_section: Option<&PluginOverrideEntry>) -> CapabilitySet {
        let manifest_caps = self.manifest.declared_effects.clone()
            .map(|c| c.into_capability_set())
            .unwrap_or_default();

        match override_section {
            Some(o) if o.plugin_id == self.id => {
                let user_caps = o.capabilities.as_ref()
                    .map(|c| c.into_capability_set())
                    .unwrap_or_default();
                // Intersection: only allow effects in BOTH manifest and user.
                // EXCEPT user-only flags (e.g., spawn-new-identities) are explicit elevations
                // and override the manifest narrowing on the flag dimension only.
                manifest_caps.intersect_with_user_override(user_caps)
            }
            _ => manifest_caps,   // No override; manifest-declared.
        }
    }
}
```

`CapabilitySet::intersect_with_user_override` (new method in `pattern_core::capability`):

```rust
impl CapabilitySet {
    /// Apply user override:
    /// - Effect categories: intersection of manifest + user (user can only narrow effects).
    /// - Resources (per-id allow-lists like has_mcp_server / has_port): intersection.
    /// - Flags: union (user can grant additional flags the manifest didn't request).
    pub fn intersect_with_user_override(&self, user: CapabilitySet) -> CapabilitySet {
        CapabilitySet {
            categories: self.categories.intersection(&user.categories).copied().collect(),
            resources: intersect_resources(&self.resources, &user.resources),
            flags: self.flags.union(&user.flags).copied().collect(),
        }
    }
}
```

The reasoning: users can never grant the plugin *more* effect categories than its manifest asked for (manifest is the upper bound), but can grant *flags* (which are user-controlled trust elevations like SpawnNewIdentities). User narrowing the resources list is honored.

At plugin enable, the runtime queries `effective_capabilities` and uses the result to build the `CapabilitySet` passed to plugin-spawned agents' prelude filtering (existing path, AC7.5 — `build_for(caps)` filters effect decls).

Empty intersection: `CapabilitySet::default()` — pure-computation only. AC7.6.

**Testing:**
Tests must verify each AC listed above:
- AC7.2: Plugin manifest declares `effects { memory; message }`; agent code tries to call `Pattern.Shell.Execute`; tidepool-extract compile fails with "Pattern.Shell.Execute not in scope" (existing prelude filter mechanism, exercised via the new flow).
- AC7.3: User KDL narrows to `effects { message }`; agent that previously could call `Memory.Put` now fails to compile.
- AC7.3 expand: User KDL adds flag `spawn-new-identities` not in manifest; effective capabilities include it.
- AC7.5: Identical to AC7.2 — verify the prelude-filtering compile-time enforcement.
- AC7.6: Plugin manifest declares `effects {}` (empty); empty `CapabilitySet`; agent program can only do pure functions (no effects); tidepool-extract compiles a pure program but fails on any effect call.

Test fixtures: a Pattern-native plugin manifest declaring various capability subsets + accompanying agent programs that should/should-not compile.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::capabilities`.

**Commit:** `[pattern-memory] [pattern-runtime] [pattern-core] add per-plugin capability overrides via plugin_overrides KDL block`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 4-6) -->

<!-- START_TASK_4 -->
### Task 4: Constellation backlink XRPC client

**Verifies:** Foundation for AC6.6 remote (counterpart-discovery path).

**Files:**
- Create: `crates/pattern_runtime/src/atproto/constellation.rs` — XRPC client wrapper for `blue.microcosm.links.getBacklinks`.
- Create: `crates/pattern_runtime/src/atproto.rs` — module root, re-exports.
- Modify: `crates/pattern_runtime/src/lib.rs` — `pub mod atproto;`.

**Note on naming:** Pattern's existing internal "Constellation" (the agent-grouping abstraction at `pattern_core::constellation::ConstellationRegistry`) is unrelated to the atproto Constellation service. Module name `atproto::constellation` keeps the boundary explicit; types use `MicrocosmLinksClient` (or similar) to disambiguate when read in isolation.

**Implementation:**

Constellation is a public hosted XRPC service at `https://constellation.microcosm.blue` (operated by the atproto community). Anonymous queries; no auth. The wrapper uses jacquard's XRPC machinery.

```rust
// pattern_runtime::atproto::constellation
use jacquard_common::types::{Did, Nsid, Rkey, BosStr, DefaultStr};
use jacquard::client::Client;
use serde::{Deserialize, Serialize};

const CONSTELLATION_URL: &str = "https://constellation.microcosm.blue";

/// Backlink record reference returned by Constellation. Pattern hydrates
/// each via `jacquard.get_record(at://<did>/<collection>/<rkey>)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacklinkRef {
    pub did: String,
    pub collection: String,
    pub rkey: String,
}

#[derive(Debug, Clone)]
pub struct BacklinksPage {
    pub total: u32,
    pub records: Vec<BacklinkRef>,
    pub cursor: Option<String>,
}

#[derive(Debug)]
pub struct ConstellationClient {
    http: reqwest::Client,
    base_url: String,
}

impl ConstellationClient {
    /// Construct against the public Constellation instance. Override the URL
    /// for testing against a local mock or a self-hosted instance.
    pub fn new() -> Self { Self::with_url(CONSTELLATION_URL.into()) }
    pub fn with_url(base_url: String) -> Self {
        // Set a stable User-Agent so Constellation's operator (fig / microcosm.blue)
        // can attribute Pattern's traffic in their metrics. Format:
        //     "pattern/<version> (+https://github.com/orual/pattern)"
        let ua = format!("pattern/{} (+https://github.com/orual/pattern)", env!("CARGO_PKG_VERSION"));
        let http = reqwest::Client::builder()
            .user_agent(ua)
            .build()
            .expect("reqwest::Client::builder() with static UA cannot fail");
        Self { http, base_url }
    }

    /// Query backlinks for a given (subject, source) pair.
    /// `subject` is the value being targeted (e.g., "node:01a3f4...").
    /// `source` is the field path (e.g., "systems.atproto.plugin.session:nodeUri").
    pub async fn get_backlinks(
        &self,
        subject: &str,
        source: &str,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<BacklinksPage, ConstellationError> {
        let url = format!("{}/xrpc/blue.microcosm.links.getBacklinks", self.base_url);
        let mut req = self.http.get(&url).query(&[("subject", subject), ("source", source)]);
        if let Some(l) = limit { req = req.query(&[("limit", &l.to_string())]); }
        if let Some(c) = cursor { req = req.query(&[("cursor", c)]); }
        let response: BacklinksRawResponse = req.send().await?.error_for_status()?.json().await?;
        Ok(BacklinksPage {
            total: response.total,
            records: response.records,
            cursor: response.cursor,
        })
    }

    /// Convenience: paginate through all backlinks (useful when result count is small).
    pub async fn get_all_backlinks(&self, subject: &str, source: &str)
        -> Result<Vec<BacklinkRef>, ConstellationError>
    { /* loop with cursor; cap at sane upper bound */ }
}

#[derive(Debug, Deserialize)]
struct BacklinksRawResponse {
    total: u32,
    records: Vec<BacklinkRef>,
    cursor: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConstellationError {
    #[error("HTTP transport: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Constellation returned an error response: {status}")]
    BadStatus { status: u16 },
}
```

**Note on jacquard XRPC.** vodplace uses `jacquard_derive::XrpcRequest` to declare the request type and pipes it through `client.xrpc(constellation_uri).send(&query)`. If Pattern's existing jacquard wiring (Phase 7 Task 5) carries a session client, prefer that path over a bare `reqwest::Client`. The implementation above is the simplest standalone shape; the executor picks whichever fits cleanly with Phase 7 Task 5's jacquard surface.

**Testing:**
- Mock the Constellation URL via wiremock. Stub `/xrpc/blue.microcosm.links.getBacklinks` with a fixture response. Call `client.get_backlinks(...)`; assert response parsed correctly.
- Pagination: stub two pages with cursors; assert `get_all_backlinks` aggregates.
- Error path: stub a 500; assert `ConstellationError::BadStatus { status: 500 }`.
- DO NOT run integration tests against the real `constellation.microcosm.blue` in CI — flaky, depends on external availability.

**Verification:**
Run: `cargo nextest run -p pattern-runtime atproto::constellation`.

**Commit:** `[pattern-runtime] add Constellation XRPC client wrapper for blue.microcosm.links.getBacklinks`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Jacquard wiring — record publish + resolve + verify

**Verifies:** Foundation for AC6.6 remote.

**Files:**
- Create: `crates/pattern_runtime/src/plugin/atproto.rs` — record schema, publish, resolve, verify helpers.
- Create: `crates/pattern_core/src/atproto.rs` (or extend existing module) — `PluginSessionRecord` type.
- Modify: workspace `Cargo.toml` — confirm `jacquard` features cover the OAuth/CredentialSession path the runtime needs.

**Implementation:**

```rust
// pattern_core::atproto
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSessionRecord {
    /// URI-shaped string formed from the iroh node ID.
    /// Example: "node:01a3f4b2..." — Constellation indexes on this field.
    #[serde(rename = "$type")]
    pub type_: String,                                // always "systems.atproto.plugin.session"
    pub node_uri: String,
    pub created_at: jiff::Timestamp,
    /// Base64-encoded ed25519 signature over the canonical dag-cbor of
    /// `{ "$type": ..., "nodeUri": ..., "createdAt": ... }` (sig field excluded).
    pub sig: String,
}
```

`plugin::atproto` operations:

```rust
use jacquard::{AgentSessionExt, CredentialSession};
use jacquard_repo::commit::{SigningKey, UnsignedCommit};
use ed25519_dalek::SigningKey as EdSigningKey;

pub async fn publish_session_record(
    session: &CredentialSession,
    plugin_id: &str,
    node_uri: &str,
    signing_key: &EdSigningKey,
) -> Result<jacquard_common::types::AtUri<'static>, AtprotoError> {
    // 1. Build the unsigned record body.
    let mut body = serde_json::json!({
        "$type": "systems.atproto.plugin.session",
        "nodeUri": node_uri,
        "createdAt": jiff::Timestamp::now().to_string(),
    });

    // 2. Compute canonical dag-cbor of the body (without sig field).
    let canonical = serde_ipld_dagcbor::to_vec(&body)?;

    // 3. Sign with ed25519.
    let signature = signing_key.sign(&canonical);
    body["sig"] = serde_json::Value::String(base64::encode(signature.to_bytes()));

    // 4. Use jacquard's create_record to publish.
    let request = jacquard::api::create_record::CreateRecord::new()
        .repo(session.session_info().did.clone())
        .collection("systems.atproto.plugin.session".into())
        .record(body.into())
        .build();
    let output = session.send(request).await?;

    Ok(output.uri)
}

pub async fn resolve_and_verify(
    session: &CredentialSession,
    record_uri: &jacquard_common::types::AtUri<'_>,
    expected_node_uri: &str,
) -> Result<PluginSessionRecord, AtprotoError> {
    // 1. Fetch via jacquard.get_record.
    let response = session.get_record::<systems_atproto_PluginSessionCollection>(record_uri).await?;
    let record: PluginSessionRecord = response.into_record();

    // 2. Verify nodeUri matches expected.
    if record.node_uri != expected_node_uri {
        return Err(AtprotoError::NodeUriMismatch {
            expected: expected_node_uri.into(),
            actual: record.node_uri.clone(),
        });
    }

    // 3. Resolve the publisher's DID document → public key.
    let did = record_uri.authority().as_did();
    let pubkey = jacquard_identity::resolve_pubkey(did).await?;

    // 4. Reconstruct unsigned canonical body, verify signature.
    let mut body = serde_json::to_value(&record)?;
    let sig_str = body.as_object_mut().unwrap().remove("sig")
        .ok_or(AtprotoError::MissingSignature)?;
    let canonical = serde_ipld_dagcbor::to_vec(&body)?;
    let sig_bytes = base64::decode(sig_str.as_str().unwrap())?;
    let sig = ed25519_dalek::Signature::try_from(&sig_bytes[..])?;
    pubkey.verify_strict(&canonical, &sig)?;

    Ok(record)
}
```

`AtprotoError` enum captures network failures, signature mismatches, missing fields, etc., per the project's `thiserror`+`#[non_exhaustive]` convention. Variant names: `NodeUriMismatch`, `MissingSignature`, `SignatureMismatch`, `Transport`, `Identity`.

**Testing:**
Tests must verify the publish + resolve + verify round trip. Use a mock PDS via jacquard's testing harness (or wiremock) — full integration test in the smoke test (Task 7).
- Publish a record; resolve via the returned URI; assert the body matches input.
- Tamper with the sig field after publish; assert resolve_and_verify returns `SignatureMismatch`.
- Tamper with the nodeUri field; assert `NodeUriMismatch`.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::atproto`.

**Commit:** `[pattern-core] [pattern-runtime] add atproto plugin session record schema + publish/resolve/verify via jacquard`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Per-plugin keypair management + atproto auth wiring at install/connect

**Verifies:** AC6.6 (remote).

**Files:**
- Modify: `crates/pattern_runtime/src/plugin/auth.rs` (created in Phase 6) — add atproto-auth code path on top of iroh node-identity allow-list.
- Modify: `crates/pattern_runtime/src/plugin/registry.rs` — `install_with_atproto_auth` flow.
- Modify: `crates/pattern_runtime/src/plugin/transport/out_of_process.rs` — verify atproto record at connect time when plugin manifest declares atproto auth.

**Implementation:**

Plugin manifest declares atproto auth via:

```kdl
transport {
    out_of_process {
        binary "/path/to/plugin"
    }
    auth "atproto" {
        plugin_did "did:plc:abc..."         // counterpart's DID, pinned at install
        // or
        // discover_via "constellation_backlink"  // backlink-based discovery
    }
}
```

At install (`PluginRegistry::install_with_atproto_auth`):

1. Generate Ed25519 keypair for the plugin via `ed25519_dalek::SigningKey::generate(&mut rng)`.
2. Store private half in keyring: `keyring::Entry::new("pattern.plugin.<plugin-id>.atproto", "default").set_password(&base64(privkey))`.
3. Compute the URI-shaped node identifier from this keypair (e.g., `node:<z-base32-pubkey>`).
4. Publish the session record on Pattern's user PDS via Task 5's `publish_session_record`. Capture the returned AT URI.
5. Store on `LoadedPlugin`:
   - `atproto_record_uri: Option<AtUri>` — our published record.
   - `expected_counterpart_did: Option<Did>` — from manifest, when pinned.
   - `discovery_mode: AtprotoDiscoveryMode` — `Pinned(did)` or `ConstellationBacklink`.
   - `node_uri: SmolStr` — the URI-shaped node identifier (e.g., `node:01a3f4...`).
   - `node_pubkey: iroh::PublicKey` — for the Phase 6 allow-list and atproto verification.
6. Register the node-pubkey in the Phase 6 allow-list (existing path).

At connect (`OutOfProcessPluginConnection::handshake`):

1. iroh QUIC accept produces a peer pubkey.
2. Phase 6 check: pubkey in registry's allow-list? If not, reject.
3. **NEW Phase 7 check** if plugin manifest declares atproto auth — branch on `discovery_mode`:

   **Pinned-DID path** (`AtprotoDiscoveryMode::Pinned(did)`):
   - Compute the AT URI of the counterpart's record at `at://<did>/systems.atproto.plugin.session/<rkey>` — `rkey` is a deterministic function of the node URI (e.g., the z-base32 component of `node:<...>`), or stored on `LoadedPlugin` at install time.
   - Call `resolve_and_verify(record_uri, expected_node_uri = node_uri)`.

   **Constellation-backlink path** (`AtprotoDiscoveryMode::ConstellationBacklink`):
   - Use Task 4's `ConstellationClient::get_backlinks(subject = node_uri, source = "systems.atproto.plugin.session:nodeUri")`.
   - For each `BacklinkRef { did, collection, rkey }` returned, build `at://<did>/<collection>/<rkey>` and call `resolve_and_verify(...)`. First successful verification wins.
   - If zero candidates verify, reject.

4. On success, accept connection. On failure, reject + log with structured fields (`plugin_id`, `discovery_mode`, `failure_reason`).

**Testing:**
Tests must verify each AC listed above:
- AC6.6 remote (pinned DID): Mock PDS via wiremock. Install plugin with `plugin_did "did:plc:test-plugin..."`. Plugin connects, presents matching node URI + signed record on its DID's PDS. Verify accept.
- Negative: tamper plugin's record sig before connect; verify reject with `SignatureMismatch` error.
- Negative: plugin presents a node URI that doesn't match its record's `nodeUri` field; verify reject with `NodeUriMismatch`.
- Constellation-backlink discovery: stub `https://constellation.microcosm.blue/xrpc/blue.microcosm.links.getBacklinks` (via wiremock) to return one `BacklinkRef` for the plugin's node URI. Stub the corresponding PDS to return the signed record. Install without pinned DID; connect; verify success.
- Constellation returns zero candidates: verify reject with clear "no verified counterpart found" error.

**Verification:**
Run: `cargo nextest run -p pattern-runtime plugin::auth::atproto`.

**Commit:** `[pattern-runtime] [pattern-core] add atproto per-plugin auth — keypair, record publish, connect-time verify`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 7-8) -->

<!-- START_TASK_7 -->
### Task 7: End-to-end smoke test at `tests/plugin_smoke.rs`

**Verifies:** AC8.1, AC8.2, AC8.4, AC8.5; AC7.1 verified end-to-end.

**Files:**
- Create: `crates/pattern_runtime/tests/plugin_smoke.rs`.
- Reuse fixtures: `tests/fixtures/plugins/cc-adapter-fixture/` (Phase 3), `cc-full-fixture/` (Phase 4), `oop-fixture/` (Phase 6), `mcp/echo-server/` (Phase 5).

**Implementation:**

```rust
#[tokio::test]
async fn plugin_smoke_full_stack() {
    let env = TestEnv::with_dual_alpn().await;

    // 1. Install CC plugin (skills + hooks + commands).
    env.registry.install_local_path(
        Path::new("tests/fixtures/plugins/cc-full-fixture"),
        PluginScope::Global,
        &env.jj,
    ).await.unwrap();
    env.registry.enable("cc-full-fixture").await.unwrap();

    // AC7.1: skill trust tier
    let foo_skill = env.memory_store.get_block_metadata::<SkillMetadata>("cc-full-fixture/foo").await.unwrap();
    assert_eq!(foo_skill.trust_tier, SkillTrustTier::PluginInstalled);

    // 2. Install IRPC native plugin (out-of-process).
    build_fixture_plugin("oop-fixture").expect("build OOP fixture");
    env.registry.install_local_path(
        Path::new("tests/fixtures/plugins/oop-fixture"),
        PluginScope::Global,
        &env.jj,
    ).await.unwrap();
    env.registry.enable("oop-fixture").await.unwrap();

    // 3. Install McpPluginAdapter wrapping fixture echo server.
    let mcp_manifest = r#"
        name "echo-mcp-bridge"
        mcp_servers {
            server "echo" {
                transport "stdio"
                command "tests/fixtures/mcp/echo-server/server.sh"
            }
        }
    "#;
    let mcp_plugin_id = env.install_native_manifest_inline(mcp_manifest).await.unwrap();
    env.registry.enable(&mcp_plugin_id).await.unwrap();

    // 4. Open a session against the mount with all three plugins enabled.
    let mock = MockProviderClient::with_turns(vec![
        // Turn 1: agent calls a CC plugin port. Hooks fire.
        Turn { assistant_text: "Calling plugin port.".into(), tool_uses: vec![tool_use_port_call("oop-fixture", "echo", json!({"value": "hi"}))] },
        // Turn 2: agent calls Pattern.Mcp.Call against the runtime-managed echo server.
        Turn { assistant_text: "Calling MCP.".into(), tool_uses: vec![tool_use_mcp_call("echo", "echo", json!({"value": "world"}))] },
        // Turn 3: agent attempts effect outside its capability set.
        Turn { assistant_text: "Trying disallowed effect.".into(), tool_uses: vec![tool_use_shell_execute("rm -rf /")] },
    ]);
    let session = env.open_session_with_mock_provider(mock).await;

    let mut hook_events: Arc<parking_lot::Mutex<Vec<HookEvent>>> = Default::default();
    let hook_collector = hook_events.clone();
    session.ctx.hook_bus.subscribe_notifications(HookFilter::new("**").unwrap(), move |event| { hook_collector.lock().push(event); }, Default::default());

    // Drive turn 1.
    session.send_message("call the plugin").await.unwrap();
    session.run_until_stop().await.unwrap();
    let events_after_t1 = hook_events.lock().clone();
    // AC8.1: hook events fire (turn.before, tool.before(port.call), port.called, tool.after, turn.after.success)
    assert!(events_after_t1.iter().any(|e| e.tag == tags::TURN_BEFORE));
    assert!(events_after_t1.iter().any(|e| e.tag == tags::PORT_CALLED));
    assert!(events_after_t1.iter().any(|e| e.tag == tags::TURN_AFTER_SUCCESS));

    // Drive turn 2 — verify MCP inverted surface.
    let composed = env.compose_next_request(&session).await;
    // AC5 verified: server reminder in segment 2.
    assert!(composed.segment2.contains("[mcp:server-available] echo"));

    session.send_message("call MCP").await.unwrap();
    session.run_until_stop().await.unwrap();
    let mcp_call_event = hook_events.lock().iter().find(|e| e.tag == tags::TOOL_AFTER && e.payload["tool_name"] == "Pattern.Mcp.Call").cloned();
    assert!(mcp_call_event.is_some(), "MCP call event missing");

    // Drive turn 3 — capability enforcement should reject.
    session.send_message("disallowed").await.unwrap();
    let err = session.run_until_stop().await.unwrap_err();
    // AC7.5 verified: shell effect rejected because not in capability set
    assert!(err.to_string().contains("PERMISSION_DENIED") || err.to_string().contains("not in scope"));

    // 5. Verify port registration.
    assert!(env.port_registry.get(&"http".into()).is_some());                // baseline runtime port
    assert!(env.port_registry.get(&"mcp:echo".into()).is_some());            // McpPluginAdapter port
    // OOP plugin's declared ports also visible — verified by Phase 6 tests; assert presence here:
    let oop_plugin = env.registry.get("oop-fixture").unwrap();
    let oop_ports = oop_plugin.connection.declare_ports().await.unwrap();
    for port_decl in &oop_ports {
        assert!(env.port_registry.get(&port_decl.id.clone()).is_some());
    }

    // AC8.5: concurrent test isolation — verified by tempdir-per-test convention; nothing to assert.
}
```

Helpers (`tool_use_port_call`, `tool_use_mcp_call`, `tool_use_shell_execute`) live alongside in `tests/support/plugin_smoke_helpers.rs`. The mock provider's `tool_use_turn` builder constructs Anthropic-shaped tool_use events.

**Testing:** the integration test above is the proof.

**Verification:**
Run: `cargo nextest run -p pattern-runtime --test plugin_smoke`.

**Commit:** `[pattern-runtime] add full-stack plugin smoke test exercising AC8 + capability enforcement`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Audit + cleanup — delete `crates/pattern_mcp/`, update CLAUDE.md

**Verifies:** AC8.3.

**Files:**
- Delete: entire `crates/pattern_mcp/` directory (recursively).
- Modify: workspace `Cargo.toml` — confirm pattern_mcp is NOT in `[workspace].members` (it isn't currently — verify).
- Modify: project `CLAUDE.md` — bump "Last verified" date, add v3-extensibility entry to status block, remove the line referencing pattern_mcp under "Retired / out-of-workspace crates".
- Audit: run `rg -F 'todo!()'`, `rg -F 'unimplemented!()'`, `rg -n '^// TODO'`, `rg -n 'blocked on'` across `crates/pattern_runtime` + `crates/pattern_core` + `crates/pattern_memory` + `crates/pattern_provider` + `crates/pattern_server` + `crates/pattern_cli` + `crates/pattern_db` + `crates/pattern_plugin_sdk`. Address each hit (fix or document deferral with explicit phase reference).

**Implementation:**

```bash
# 1. Delete the orphan directory.
rm -rf crates/pattern_mcp/

# 2. Audit for leftover stubs / TODOs.
rg -F 'todo!()' crates/
rg -F 'unimplemented!()' crates/
rg -n '^[[:space:]]*// TODO' crates/
rg -n 'blocked on' crates/ docs/
```

For each hit: address (preferred) or document why deferral is valid (link to a follow-up plan or AC slot). The project guidance is unambiguous: "Documenting a gap is never a fix"; if the audit finds wired-but-stubbed code in this implementation plan's surface, fix it.

CLAUDE.md updates:

```diff
-Last verified: 2026-04-26
+Last verified: 2026-04-27
```

In the "Current State" section, append:

```markdown
v3-extensibility (7 phases) complete: plugin manifest + registry, hook lifecycle (open-tag dispatch with 80+ tag catalog), CC plugin adapter (skills, agents, monitors, commands, bin, .mcp.json → Pattern McpServerConfig translation, Pattern.Cc Haskell compat), MCP inverted surface (system reminders in segment 2, tool docs as Skill blocks at `mcp/<server>/<tool>`), plugin transport (in-process via direct trait dispatch + out-of-process via iroh QUIC; Router ALPN multiplexing on `pattern-plugin/1` and `pattern-plugin-memory-sync/1`), `pattern-plugin-sdk` crate (slim dep graph; default-features = false), McpPluginAdapter wrapping standalone MCP servers, opaque atproto plugin auth records signed in canonical dag-cbor (NSID `systems.atproto.plugin.session`; counterpart discovery via Constellation backlinks at `constellation.microcosm.blue`), ad-hoc skill body-redaction with user-enable flow, per-plugin capability overrides via `plugin_overrides {}` KDL block. `crates/pattern_mcp/` deleted; MCP client lives in `pattern_runtime::mcp`.
```

Remove the `pattern_mcp/ — MCP client/server (pre-v3 shape)` bullet from the Retired list section.

Update Cargo.toml's workspace members if any stale references exist (none expected; pattern_mcp is already out).

**Testing:**
- `cargo nextest run --workspace` green.
- `cargo build --workspace` clean (pattern_mcp's absence doesn't break anything).
- Audit greps return no unresolved hits introduced by this plan's work.

**Verification:**
Run: `just pre-commit-all` (format + clippy + workspace build + tests).

**Commit:** `[meta] [pattern-runtime] post-v3-extensibility audit + delete pattern_mcp/ + CLAUDE.md refresh`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_D -->

---

## Phase done-when checklist

- [ ] `SkillMetadata.enabled` field exists; `AdHoc` skills default to `false`; `Load` handler redacts unenabled bodies.
- [ ] `skill_approvals` table + `enable_skill` RPC + CLI `pattern skills enable <label>` + `/skill-enable` slash command all work end-to-end.
- [ ] `.pattern.kdl` `plugin_overrides {}` block parses; `LoadedPlugin::effective_capabilities()` computes intersection (effects narrow, flags expand).
- [ ] `CapabilitySet::intersect_with_user_override` lands in `pattern_core`.
- [ ] `ConstellationClient` XRPC wrapper in `pattern_runtime::atproto::constellation` queries `blue.microcosm.links.getBacklinks` correctly.
- [ ] Jacquard wired: `publish_session_record` + `resolve_and_verify` + `PluginSessionRecord` schema. Records are opaque (`nodeUri` + `createdAt` + `sig` only). NSID is `systems.atproto.plugin.session`.
- [ ] Per-plugin keypair generated at install, private stored in keyring, public + record URI on `LoadedPlugin`.
- [ ] At connect (out-of-process), atproto record verification layered over Phase 6 iroh node-id allow-list when manifest declares atproto auth.
- [ ] `tests/plugin_smoke.rs` exercises CC plugin + IRPC native + MCP plugin adapter + hook events + MCP inverted surface + capability enforcement; all assertions pass.
- [ ] `crates/pattern_mcp/` deleted from disk.
- [ ] Project CLAUDE.md updated: "Last verified" + v3-extensibility status entry + retired-crates list cleaned.
- [ ] No residual `todo!()` / `unimplemented!()` / `// TODO` introduced by v3-extensibility phases; audit greps clean.
- [ ] `cargo nextest run --workspace` green.
- [ ] `just pre-commit-all` clean.

---

## Notes for executor

- **Phase 7 is wide.** Eight tasks span ad-hoc trust + capability overrides + atproto auth + smoke + cleanup. Each subcomponent is independently testable; the smoke test (Task 7) integrates all of them. Plan to land subcomponents A → B → C → D in order; subcomponent C (atproto auth) is the longest individual stretch.
- **Atproto auth scope.** Phase 7 ships:
  - Same-machine local plugins: existing iroh node-id allow-list (Phase 6) is sufficient.
  - Cross-machine plugins: opaque records + jacquard publish/resolve/verify + node-pubkey verification.
  - Counterpart discovery: pinned-DID (manifest declares the counterpart's DID) and Constellation-backlink (queries `blue.microcosm.links.getBacklinks` on the public Constellation service). No Pattern-side firehose subscription, no Pattern-side index.
- **Constellation naming hazard.** Pattern's existing internal `ConstellationRegistry` (the agent-grouping abstraction) is unrelated to the atproto Constellation service. Use `pattern_runtime::atproto::constellation` for the XRPC client; never re-export it as `pattern_runtime::Constellation` or anywhere it could collide. Tests, modules, types: prefer `MicrocosmLinksClient` or `AtprotoBacklinks*` naming if any ambiguity surfaces in code review.
- **`MemoryStore::update_block_metadata`.** If this method doesn't yet exist on the trait, add it as part of Task 1. Per project guidance: extending storage APIs cleanly is in scope.
- **Per project guidance: do NOT defer ad-hoc skill redaction.** The investigator confirmed it's load-bearing — without redaction, `Memory.Put`-installed skills expose their bodies to agents without partner consent. AC7.4 says this must work; Phase 7 is where it lands.
- **Plugin keypair lifecycle.** Storing in keyring keeps the private key out of disk-cleartext but creates a per-machine binding (uninstall on machine A doesn't propagate; the key dies with the keyring entry). Document in `pattern_runtime/CLAUDE.md` after Phase 7.
- **`crates/pattern_mcp/` deletion is permanent.** Use `git rm -r` or `jj` equivalent — not `rm -rf` so the deletion is staged in version control.
- **The smoke test in Task 7 is intentionally wide.** Per the design plan's notes: "Per-phase tests isolate regressions; the smoke test proves composition. Both are load-bearing." If a per-phase test is missing for a behavior the smoke test exercises, add the per-phase test BEFORE shipping the smoke test — don't rely on the smoke as a substitute.
- **The audit task (Task 8) should fail loudly if it finds stub residue.** Don't hand-wave a `todo!()` because it's "in a deferred area"; document the deferral inline with a phase reference, or fix it. Per project guidance.
- **Future work after v3-extensibility ships:**
  - Pattern-side atproto record cache (offline-tolerant counterpart discovery when Constellation is unreachable).
  - Plugin marketplace + discovery.
  - Cross-device constellation coordination.
  - MCP server (Pattern exposes MCP to other clients).
  - `Pattern.Mcp.Subscribe` for resource subscriptions.
  - Auto-reconnect for out-of-process plugins after process death.
  - Periodic key rotation for atproto auth records.
  - `prompt`-type CC hooks via `Message.Ask` un-stub.
  - WASM plugin transport.
  These are explicitly NOT in scope for this plan.
