# v3-task-skill-blocks Phase 4: Skill schema + md+YAML frontmatter

**Goal:** Add a `BlockSchema::Skill` variant, introduce YAML-frontmatter + markdown-body canonical files, assign trust tiers to skill provenance, and keep runtime-captured usage stats separate from the canonical file so agent loads don't bust the content-hash echo suppression.

**Architecture:** Skill blocks pair a YAML frontmatter metadata region with a markdown body. Canonical files live at `<mount>/skills/<name>.md` (or inside block-owned project scope). The LoroDoc root carries `schema: "skill"`, a `metadata` LoroMap (author-defined), `extras` LoroMap (unknown frontmatter keys, preserved for round-trip), and a `body` LoroText. Runtime usage stats (`last_used`, `last_used_by`, `use_count`) live in a dedicated sqlite table (`skill_usage_stats`) — NOT in the LoroDoc — because they are per-local-install observability that doesn't belong in replicated content. This keeps the canonical `.md` file content-hash-stable across load events without needing any special "skip this subtree" emitter carve-out. Frontmatter parses via `saphyr` 0.0.6 using a hand-written visitor, matching the project's existing "hand-written AST↔LoroValue converter" convention from the sibling KDL work.

**Tech Stack:** Rust (pattern_core, pattern_memory, pattern_db), `saphyr = "0.0.6"` (new workspace dep), `loro`, `serde_json` (for opaque `hooks`), `rusqlite`, `metrics` 0.23 (already available per sibling), `thiserror`.

**Scope:** Phase 4 of 5.

**Codebase verified:** 2026-04-19.

---

## Acceptance Criteria Coverage

### v3-task-skill-blocks.AC6: Skill schema + md+YAML frontmatter round-trip

- **v3-task-skill-blocks.AC6.1 Success:** `BlockSchema::Skill { expected_keys }` exists and is exported
- **v3-task-skill-blocks.AC6.2 Success:** Round-trip property test: arbitrary `SkillMetadata` + arbitrary markdown body serialize to .md+frontmatter, parse back, LoroValue matches original
- **v3-task-skill-blocks.AC6.3 Success:** Saphyr parses valid YAML frontmatter without panics; unknown keys preserved in the loro state for round-trip even if not in `SkillMetadata`
- **v3-task-skill-blocks.AC6.4 Success:** `SkillMetadata.hooks` preserves nested structure as `serde_json::Value` through round-trip
- **v3-task-skill-blocks.AC6.5 Failure:** Malformed YAML frontmatter (syntax error, missing required `name`) produces `SkillParseError` with file location
- **v3-task-skill-blocks.AC6.6 Failure:** Missing frontmatter delimiters (`---`) rejects with a clear error
- **v3-task-skill-blocks.AC6.7 Edge:** Frontmatter with all-optional fields (only `name` + `trust_tier` set) parses correctly with None / empty defaults

### v3-task-skill-blocks.AC7: Trust tier assignment

- **v3-task-skill-blocks.AC7.1 Success:** Skill loaded from pattern_runtime's SDK resource directory → `trust_tier == FirstParty`
- **v3-task-skill-blocks.AC7.2 Success:** Skill loaded from `<mount>/skills/foo.md` → `trust_tier == ProjectLocal`
- **v3-task-skill-blocks.AC7.3 Success:** Skill block created at runtime via `MemoryStore::put_block` → `trust_tier == AdHoc`
- **v3-task-skill-blocks.AC7.4 Success:** Frontmatter declaring `trust_tier: "plugin-installed"` on a file loaded from `<mount>/skills/` preserves the `PluginInstalled` value on round-trip (no overwrite to `ProjectLocal`)
- **v3-task-skill-blocks.AC7.5 Success:** Loading a skill with declared `PluginInstalled` tier increments `metrics::counter!("skill.plugin_installed_tier_without_plugin_system")` and logs a warning
- **v3-task-skill-blocks.AC7.6 Edge:** A skill with invalid `trust_tier` string value in frontmatter (e.g., `"foo"`) surfaces a parse error; does NOT silently default to `AdHoc`

---

## Design deviations recorded during planning

- **saphyr version:** design says "saphyr" without a version. Use `saphyr = "0.0.6"` (June 2025 release, actively maintained; depends on `arraydeque`, `hashlink`, `saphyr-parser` — minimal transitive footprint).
- **SDK resource directory:** the design references an SDK resource directory for FirstParty skills but none exists yet. Task 6 creates `crates/pattern_runtime/resources/skills/` and adds an initial `README.md` placeholder. Runtime code resolves this path at compile time via `CARGO_MANIFEST_DIR`-relative constant or at runtime by walking from the pattern_runtime binary's `env::current_exe()` to find a co-located `resources/skills/`. For simplicity this plan uses a compile-time `const FIRST_PARTY_SKILL_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/skills")` constant.
- **Usage stats are sqlite-only, NOT in the LoroDoc.** The design text puts `SkillUsageStats` partly in the LoroDoc's metadata region. This plan diverges: usage stats live in a dedicated `skill_usage_stats` sqlite table (handle-keyed), updated via a direct pattern_db query. Rationale:
  - Usage stats are per-local-install observability (how often *this* runtime has loaded a skill). They're not replicated content — two nodes with divergent use counts shouldn't merge via CRDT semantics.
  - Putting them in the LoroDoc would force a chain of special cases: a new mutation hook on `MemoryStore` (not in the sibling trait), an emitter carve-out so usage writes don't touch the canonical `.md`, and a coordination item with the sibling plan. sqlite-only is strictly simpler.
  AC9.3 / AC9.6's "canonical file unchanged by load" property falls out automatically because the load handler never writes the file — it only writes a sqlite row.
- **Content-hash echo suppression dependency (still relevant for skill content edits):** sibling memory-rework Phase 4 implements content-hash echo suppression for the `.md`/`.kdl` emit path. When a skill author edits body or frontmatter, the file hash changes and a single emit fires. Usage-stat updates bypass this entirely by not touching the file. Task 1 re-verifies the suppression is in place for the content-edit path.
- **Phase 4 investigator flagged "drift":** the investigator confused sibling memory-rework Phase 4 (which uses KDL for Map/List/Composite) with this plan's Phase 4 (which adds YAML frontmatter parsing for a DIFFERENT block schema). No actual drift; they are different phases of different plans. No action.

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->
### Subcomponent A: Prerequisites + dependency

<!-- START_TASK_1 -->
### Task 1: Verify sibling prerequisites

**Files:** none written.

**Step 1: Confirm pattern_memory + fs module exists**

Run: `fd -e rs fs crates/pattern_memory/src`
Expected: `crates/pattern_memory/src/fs/mod.rs` + `fs/kdl.rs` + possibly a `fs/markdown.rs` for Text blocks. Record which exists.

**Step 2: Confirm content-hash echo suppression**

Run: `rg -n 'fn compute.*hash|content_hash|echo' crates/pattern_memory/src/subscriber crates/pattern_memory/src/fs 2>/dev/null`
Expected: at least one function-level match demonstrating sibling Phase 4 has landed the suppression. If missing, STOP — Phase 4 can't guarantee AC9.3 / AC9.6 behaviour otherwise.

**Step 3: Confirm BlockSchema is at the post-relocation path**

Run: `rg -n '#\[non_exhaustive\].*pub enum BlockSchema|pub enum BlockSchema' crates/pattern_core/src`
Expected: the enum lives at `pattern_core::types::memory_types::schema`.

**Step 4: No commit.**

(The sqlite-only usage-stats decision means no coordination item with the sibling plan — this phase is self-contained except for the pattern_memory crate foundation.)
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Add `saphyr` workspace dep

**Files:**
- Modify: `Cargo.toml` (workspace root).
- Modify: `crates/pattern_memory/Cargo.toml` — add `saphyr = { workspace = true }`.

**Step 1: Add to workspace**

In `[workspace.dependencies]` add:
```toml
saphyr = "0.0.6"
```

**Step 2: Verify build**

Run: `cargo check --workspace`
Expected: clean compile; saphyr transitively pulls `arraydeque`, `hashlink`, `saphyr-parser` only.

**Step 3: Commit**

```
jj commit -m "[meta] add saphyr 0.0.6 YAML parser workspace dep"
```
<!-- END_TASK_2 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->
### Subcomponent B: Types + BlockSchema variant

<!-- START_TASK_3 -->
### Task 3: `SkillMetadata`, `SkillTrustTier`, `SkillUsageStats`

**Verifies:** v3-task-skill-blocks.AC6.1 (supporting), AC6.4 (hooks shape).

**Files:**
- Create: `crates/pattern_core/src/types/memory_types/skill.rs`.
- Modify: `crates/pattern_core/src/types/memory_types/mod.rs` to add `pub mod skill;` + re-exports.

**Implementation:**

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub trust_tier: SkillTrustTier,
    pub description: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub hooks: serde_json::Value,  // opaque; skill-author-defined
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillTrustTier {
    FirstParty,
    ProjectLocal,
    PluginInstalled,
    AdHoc,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkillUsageStats {
    pub last_used: Option<Timestamp>,
    pub last_used_by: Option<AgentId>,
    pub use_count: u64,
}
```

Document in rustdoc:
- `SkillMetadata` is the author-defined content; serialized to the canonical `.md` frontmatter.
- `SkillUsageStats` is runtime-captured; NEVER serialized to the canonical file.
- `hooks` intentionally uses `serde_json::Value` to allow skill authors to embed arbitrary nested structure without schema evolution.

**Testing:**

- `SkillTrustTier` serializes to `"first-party"`, `"project-local"`, `"plugin-installed"`, `"ad-hoc"`; round-trip via serde_json.
- `SkillMetadata` with nested `hooks` object (checklist array + workflow map) round-trips byte-equal via serde_json.
- `SkillUsageStats::default()` produces all-None/0 state.
- Deserializing `{ "trust_tier": "foo" }` returns an error (AC7.6 support).

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib types::memory_types::skill`

**Commit:**
```
jj commit -m "[pattern-core] SkillMetadata, SkillTrustTier, SkillUsageStats types"
```
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Add `BlockSchema::Skill` variant + match site updates

**Verifies:** v3-task-skill-blocks.AC6.1.

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/schema.rs` (or the current BlockSchema location per Task 1).
- Modify: every `BlockSchema` match site from Phase 1 Task 3's scratch list, plus any new sites introduced between Phase 1 and Phase 4 — re-grep.

**Implementation:**

```rust
// In the BlockSchema enum:
Skill {
    expected_keys: Vec<String>,
},
```

`expected_keys` is author-declared hints about which metadata keys this block template expects — treated as soft documentation; not enforced in this plan.

For each match site:
- Helper methods on BlockSchema — treat Skill like Text for all of `is_field_read_only` (false), `read_only_fields` (empty), `is_section_read_only` (false), `get_section_schema` (None).
- pattern_cli debug printer: print `"Skill(expected_keys=[...])"`.
- pattern_memory document rendering: dispatch to the new `fs::markdown_skill` emitter. Task 4 adds the `BlockSchema::Skill { .. }` match arm that calls into the emitter module. If the emitter module is not yet landed (Tasks 6-7 do that), the arm returns a typed `Err(FsError::ConverterNotYetAvailable(BlockSchemaKind::Skill))` — no `unreachable!()` in production code. Task 7 replaces the stub return with the actual emitter call.

**Testing:**
- Serde round-trip of `BlockSchema::Skill { expected_keys: vec!["checklist".into()] }`.

**Verification:**
- Run: `cargo check --workspace`.
- Run: `cargo nextest run -p pattern-core -p pattern-cli --lib`.

**Commit:**
```
jj commit -m "[pattern-core] [pattern-cli] add BlockSchema::Skill variant"
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `SkillParseError` + position-bearing errors

**Files:**
- Create: `crates/pattern_memory/src/fs/markdown_skill/errors.rs` (or inline if the module is small — implementor's call).

**Implementation:**

```rust
use miette::SourceSpan;

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SkillParseError {
    #[error("missing frontmatter delimiters (--- ... ---)")]
    MissingDelimiters,

    #[error("YAML parse error at {span:?}: {source}")]
    Yaml { span: SourceSpan, source: saphyr::ScanError },

    #[error("missing required key `{key}`")]
    MissingRequiredKey { key: &'static str, span: Option<SourceSpan> },

    #[error("key `{key}` has wrong type: expected {expected}, got {actual}")]
    TypeMismatch { key: String, expected: &'static str, actual: &'static str, span: Option<SourceSpan> },

    #[error("invalid trust tier `{value}`")]
    InvalidTrustTier { value: String, span: Option<SourceSpan> },

    #[error("body is not valid UTF-8")]
    NonUtf8Body,
}
```

miette `SourceSpan` makes it easy to pretty-print file:line references.

**Testing:** deferred to Task 7 (tests the error paths end-to-end).

**Commit:**
```
jj commit -m "[pattern-memory] SkillParseError with span-bearing variants"
```
<!-- END_TASK_5 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-8) -->
### Subcomponent C: Frontmatter converter + trust tier assignment

<!-- START_TASK_6 -->
### Task 6: `markdown_skill` converter — parse direction

**Verifies:** v3-task-skill-blocks.AC6.2 (partial — parse half), AC6.3, AC6.4, AC6.5, AC6.6, AC6.7, AC7.6 support.

**Files:**
- Create: `crates/pattern_memory/src/fs/markdown_skill/mod.rs` + (if split) `parse.rs`, `emit.rs`, `visitor.rs`.
- Modify: `crates/pattern_memory/src/fs/mod.rs` to add `pub mod markdown_skill;`.
- Create: `crates/pattern_runtime/resources/skills/README.md` with a placeholder note ("First-party skills live here.").

**Implementation — parser pipeline:**

```
parse(bytes: &[u8]) -> Result<SkillFile, SkillParseError>

1. Decode bytes as UTF-8 (NonUtf8Body error on failure).
2. Locate frontmatter delimiters:
   - Must start with `---\n`. If not, return MissingDelimiters.
   - Find the next line `---\n` after byte 4. If not, return MissingDelimiters.
   - Slice: frontmatter_src = bytes[4..next_delim_start], body = bytes[next_delim_end+4..] (skip the delimiter + newline).
3. Parse frontmatter_src via saphyr:
   - let docs = saphyr::Yaml::load_from_str(frontmatter_src)
     .map_err(|e| SkillParseError::Yaml { span: ..., source: e })?;
   - Expect exactly one document; more than one is an error (TypeMismatch on root).
4. Visit the root Yaml::Mapping with a hand-written visitor (Task 6b).
5. Return SkillFile { metadata: SkillMetadata, extras: LoroMap (unknown keys), body: String }
```

**Visitor behaviour:**
- Required keys: `name` (must be a non-empty String), `trust_tier` (must be one of the four kebab-case variants; else InvalidTrustTier).
- Optional keys: `description` (String or null), `keywords` (Sequence of Strings; empty default; reject if Sequence contains non-String), `hooks` (any Yaml value, converted via a generic Yaml→serde_json::Value helper and stored in `SkillMetadata.hooks`).
- Unknown keys: preserved to `extras` as a `LoroValue::Map` via a generic Yaml→LoroValue converter. Conversion rules:
  - Yaml::Null → LoroValue::Null
  - Yaml::Boolean(b) → LoroValue::Bool(b)
  - Yaml::Integer(i) → LoroValue::I64(i) (saphyr exposes i64)
  - Yaml::Real(s) → LoroValue::Double(parse<f64>) with NaN/inf passing through
  - Yaml::String(s) → LoroValue::String(s)
  - Yaml::Array(v) → LoroValue::List
  - Yaml::Hash(m) → LoroValue::Map
  - Yaml::BadValue / alias → TypeMismatch error
  - `!!binary` tag → LoroValue::Binary (consistent with sibling memory plan's binary handling)
- Reject type mismatches loudly with `TypeMismatch` carrying the offending key.

**Testing:**

Unit tests in `markdown_skill/parse.rs`:
- Minimal valid frontmatter (only `name` + `trust_tier`) → `SkillMetadata` with None/empty defaults elsewhere (AC6.7).
- Frontmatter with nested `hooks` (checklist list + workflow map) → `hooks` serde_json::Value has the same nested shape (AC6.4).
- Frontmatter with unknown top-level key `author: "@me"` → key appears in `extras` LoroMap (AC6.3).
- Missing `---` delimiter → `MissingDelimiters`.
- Missing `name` key → `MissingRequiredKey { key: "name" }`.
- `trust_tier: "foo"` → `InvalidTrustTier { value: "foo" }` (AC7.6).
- `keywords: 42` → `TypeMismatch { key: "keywords", expected: "sequence", actual: "integer" }`.
- Invalid YAML syntax (`name: [`) → `Yaml { span, source }` with span pointing at the error.

**Verification:**
- Run: `cargo nextest run -p pattern-memory --lib fs::markdown_skill::parse`

**Commit:**
```
jj commit -m "[pattern-memory] skill frontmatter parser with saphyr AST visitor"
```
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: `markdown_skill` converter — emit direction + round-trip

**Verifies:** v3-task-skill-blocks.AC6.2 (round-trip), AC9.6 support (no VCS dirtiness).

**Files:**
- Modify: `crates/pattern_memory/src/fs/markdown_skill/mod.rs` (or `emit.rs`).

**Implementation:**

```
emit(metadata: &SkillMetadata, extras: &LoroValue, body: &str) -> Result<String, SkillEmitError>

1. Build a saphyr Yaml AST from metadata + extras:
   - Emit fields in a stable order: name, trust_tier, description, keywords, hooks, then extras keys in sorted order. Stable order is crucial for content-hash stability.
   - Render Yaml::Hash → string via saphyr's emitter or a hand-written formatter that produces canonical output (quoted strings, 2-space indent, no trailing whitespace).
2. Assemble:
   - "---\n" + yaml_string + "---\n\n" + body
3. If body doesn't already end with a newline, append one. Ensures stable round-trip.
```

**Testing:**

Proptest round-trip in `crates/pattern_memory/tests/skill_md_roundtrip.rs`:
- Generate bounded `SkillMetadata` (name non-empty, trust_tier from the 4 variants, optional description, keyword vec ≤ 5 entries, hooks as a bounded serde_json::Value).
- Generate bounded markdown body (up to 2000 chars, UTF-8 incl. multi-byte + newlines).
- Property: `parse(emit(m, extras, body)).unwrap() == (m, extras, body)`.
- At least 100 proptest cases, no shrunken counterexamples.

Unit tests:
- `emit` produces byte-identical output for the same input across 1000 calls (stable iteration order).
- `parse → emit → parse` for a fixture file containing all-nested hooks produces identical second parse.

**Verification:**
- Run: `cargo nextest run -p pattern-memory --test skill_md_roundtrip`

**Commit:**
```
jj commit -m "[pattern-memory] skill frontmatter emitter + proptest round-trip"
```
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: `assign_trust_tier` logic

**Verifies:** v3-task-skill-blocks.AC7.1, AC7.2, AC7.3, AC7.4, AC7.5.

**Files:**
- Create: `crates/pattern_memory/src/skill.rs` (if not already present — some sibling-plan work may have stubbed it).
- Modify: `crates/pattern_memory/src/lib.rs` to `pub mod skill;`.

**Implementation:**

```rust
pub struct SkillProvenance {
    pub source: SkillSource,
    pub declared_tier: Option<SkillTrustTier>,  // from frontmatter
}

pub enum SkillSource {
    SdkResourceDir,        // pattern_runtime/resources/skills
    MountSkillsDir { mount: PathBuf },
    ProjectBlock,          // stored as a block in project scope
    Runtime,               // created via put_block by an agent
}

pub fn assign_trust_tier(prov: &SkillProvenance) -> SkillTrustTier {
    use SkillSource::*;
    // If the frontmatter declared PluginInstalled explicitly, preserve it and warn.
    if prov.declared_tier == Some(SkillTrustTier::PluginInstalled) {
        metrics::counter!("skill.plugin_installed_tier_without_plugin_system").increment(1);
        tracing::warn!("skill declares trust_tier=plugin-installed but plugin system is not active (Plan 4 concern)");
        return SkillTrustTier::PluginInstalled;
    }
    // Otherwise derive from source; declared tier only preserves for plugin-installed
    // (per AC7.4 wording: plugin-installed doesn't get overwritten by ProjectLocal).
    match prov.source {
        SdkResourceDir => SkillTrustTier::FirstParty,
        MountSkillsDir { .. } | ProjectBlock => SkillTrustTier::ProjectLocal,
        Runtime => SkillTrustTier::AdHoc,
    }
}
```

Resolver helper: `resolve_source_for_path(path: &Path, known_mounts: &[&Path]) -> SkillSource` — checks FIRST_PARTY_SKILL_DIR const, then walks each known mount's `skills/` subdir, falls back to Runtime.

**Testing:**

Unit tests:
- `SkillSource::SdkResourceDir` → FirstParty (AC7.1).
- `MountSkillsDir` → ProjectLocal (AC7.2).
- `Runtime` → AdHoc (AC7.3).
- Declared PluginInstalled + MountSkillsDir source → PluginInstalled (AC7.4); metric counter incremented (use `metrics-util::debugging` recorder); log event captured via `tracing-test` or equivalent.
- Declared AdHoc + SdkResourceDir source → FirstParty (source wins for non-plugin-installed declarations; document this policy decision in rustdoc and in the design-deviations note below).

**Design-policy note (include in rustdoc):**
> Only `PluginInstalled` is preserved from the frontmatter; all other declared tiers are overridden by the source-derived tier. Rationale: project-local / first-party assertions shouldn't be forgeable by authors; plugin-installed is preserved only because Plan 4 will validate its provenance through a separate mechanism.

**Verification:**
- Run: `cargo nextest run -p pattern-memory --lib skill::assign_trust_tier`

**Commit:**
```
jj commit -m "[pattern-memory] assign_trust_tier with plugin-installed warning metric"
```
<!-- END_TASK_8 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 9-10) -->
### Subcomponent D: Document rendering + scope coverage

<!-- START_TASK_9 -->
### Task 9: Wire Skill schema into document rendering + sqlite usage stats

**Files:**
- Modify: `crates/pattern_memory/src/document.rs` (or equivalent — path from Task 1).
- Create migration: `crates/pattern_db/migrations/0015_skill_usage_stats.sql` (or next-free slot per Phase 2 Task 1's migration-layout decision).
- Create: `crates/pattern_db/src/queries/skill_usage.rs`.
- Modify: `crates/pattern_db/src/queries/mod.rs` (add `pub mod skill_usage;`).

**Migration contents:**

```sql
CREATE TABLE skill_usage_stats (
    block_handle   TEXT PRIMARY KEY NOT NULL,
    last_used      TEXT,                              -- ISO-8601 timestamp, nullable
    last_used_by   TEXT,                              -- AgentId, nullable
    use_count      INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
```

The table is orphan-tolerant: rows referring to deleted Skill blocks are harmless. Optional future cleanup (cascade on block delete) is a Plan 4 concern.

**Query functions in `skill_usage.rs`:**

```rust
pub fn record_usage(
    tx: &rusqlite::Transaction,
    block: &BlockHandle,
    agent: &AgentId,
    at: Timestamp,
) -> rusqlite::Result<()>;
// INSERT ... ON CONFLICT(block_handle) DO UPDATE
// SET last_used = excluded.last_used,
//     last_used_by = excluded.last_used_by,
//     use_count = skill_usage_stats.use_count + 1;

pub fn get_usage_stats(
    conn: &rusqlite::Connection,
    block: &BlockHandle,
) -> rusqlite::Result<SkillUsageStats>;
// Returns SkillUsageStats::default() when no row exists.

pub fn get_usage_stats_batch(
    conn: &rusqlite::Connection,
    blocks: &[BlockHandle],
) -> rusqlite::Result<HashMap<BlockHandle, SkillUsageStats>>;
// Used by Phase 5's `ctx.skills.list` to join stats into SkillInfo without N+1.
```

**Document rendering:**

- On block read (inbound, `.md` file → LoroDoc): parse via `markdown_skill::parse`; populate:
  - `schema: "skill"` at LoroDoc root.
  - `metadata` LoroMap from SkillMetadata (typed fields).
  - `extras` LoroMap from unknown-keys map.
  - `body` LoroText from body string.
  - No `usage_stats` field — they live in sqlite, not the LoroDoc.
- On block emit (outbound, LoroDoc → `.md` file): project `metadata` + `extras` back into `SkillMetadata` + `LoroValue::Map`, call `markdown_skill::emit`.

**Testing:**

- `skill_usage_stats_migration_applies_clean`: fresh DB, migration runs, table exists with the expected schema.
- `record_usage_inserts_and_increments`: call `record_usage` three times with the same block, assert `use_count == 3`, `last_used` matches the most recent call.
- `get_usage_stats_default_for_unknown_block`: query a handle with no row, assert `SkillUsageStats::default()`.
- `get_usage_stats_batch_for_mixed_presence`: call with 5 handles, 3 have rows, 2 don't; map returned contains exactly 3 entries.
- Content-hash stability check: round-trip a skill file through parse → emit → parse → emit with 100 `record_usage` calls between iterations; final emitted bytes equal initial file bytes (sqlite writes don't touch the file — trivially true, but the test documents the property). Covers AC9.3 / AC9.6 end-to-end (though Phase 5 owns the `load` handler that calls `record_usage`).
- Scope enforcement test: Skill block in project scope with `MemoryScope::Full` isolation is invisible to persona-default sessions.

**Verification:**
- Run: `cargo nextest run -p pattern-db --lib queries::skill_usage`
- Run: `cargo nextest run -p pattern-memory --test skill_md_roundtrip`

**Commit:**
```
jj commit -m "[pattern-db] [pattern-memory] Skill schema document I/O + skill_usage_stats table"
```
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: FTS5 indexing coverage for Skill blocks

**Files:**
- Modify: `crates/pattern_memory/src/subscriber/mod.rs` or the per-schema subscriber file — add a dispatch arm so Skill block commits update the FTS5 index used by the `ctx.skills.search` handler in Phase 5.
- Modify: the sibling's FTS5 migration (or add a new migration 0015 here if the sibling plan's FTS table doesn't already cover skill content) — prefer coordinating with sibling plan over stacking migrations.

**Implementation:**

Index the Skill block's name + description + keywords + body text in the existing `memory_blocks_fts` virtual table (recorded in Phase 2 investigation). Dispatch on `BlockSchema::Skill { .. }` in the subscriber; construct a search document from `metadata.name + " " + metadata.description.unwrap_or("") + " " + metadata.keywords.join(" ") + "\n" + body`. The existing FTS table already holds text content for other block schemas — extend the writer to include Skill.

**Testing:**

- `fts5_skill_search_by_name`: create skills with various names; search finds by name substring.
- `fts5_skill_search_by_description`: search hits description text.
- `fts5_skill_search_by_keyword`: search hits keywords array entries.
- `fts5_skill_search_by_body`: search hits body text.
- `fts5_skill_content_snapshot` (insta): representative 3-skill fixture produces stable BM25 ordering across runs.

**Verification:**
- Run: `cargo nextest run -p pattern-memory --lib subscriber::skill`
- Run: `cargo nextest run -p pattern-memory --test skill_fts5`

**Commit:**
```
jj commit -m "[pattern-memory] FTS5 indexing for Skill blocks"
```
<!-- END_TASK_10 -->
<!-- END_SUBCOMPONENT_D -->

---

## Phase 4 Done when

- Task 1 prerequisites pass (BlockSchema moved, fs/ module present, content-hash suppression in place).
- saphyr 0.0.6 added to workspace; `cargo check --workspace` clean.
- Skill block round-trip proptest passes.
- Trust tier assignment unit tests pass including PluginInstalled preservation + metric.
- FTS5 snapshot stable.
- `skill_usage_stats` migration applied; `record_usage` / `get_usage_stats` / `get_usage_stats_batch` query functions covered by unit tests.
- Content-hash stability verified: running `record_usage` N times does not alter the canonical `.md` bytes (follows trivially because only sqlite is touched).
- Scope enforcement test (Skill block in project scope invisible to persona) passes.
- No `TODO`, `unimplemented!()`, or commented-out code introduced.
