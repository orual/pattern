# v3-multi-agent Phase 6: Agent registry and identity

**Goal:** give every persona a first-class registry entry in `pattern_db` with status (`Active`/`Draft`/`Inactive`), config file location, and project attachments. Introduce relationship edges (`SupervisorOf`, `SpecialistFor`, `PeerWith`, `ObserverOf`) as a dedicated table replacing the old `agent_groups`/`group_members` coordination schema. Surface discovery through `ctx.constellation.{list,find,groups}`. Auto-register siblings on spawn. Consume Phase 4's in-memory draft-queue when a human promotes a draft persona, so queued messages flow into the newly-opened mailbox. Retire the staging-era `CoordinationPattern` types and the orphan `coordination_tasks` / `agent_groups` / `group_members` tables.

**Architecture:** the registry is a pair of migrations on pattern_db's memory database — one extends `agents` with the missing columns, one introduces `persona_relationships` and `persona_groups` (plus a membership join). A `ConstellationRegistry` type in `pattern_core` owns the in-memory projection (cached from DB) and exposes CRUD + query methods; the daemon holds one per runtime. Sibling spawn (Phase 2) and fronting updates (Phase 5) wire into it via clear insertion points. Promotion of a draft persona is a daemon-level RPC: open the session, flip `status` from `Draft` to `Active`, register the mailbox with the agent registry (Phase 4), replay Phase 4's draft-queue into the mailbox, emit `WireTurnEvent::FrontingChanged` if applicable.

**Tech Stack:** `rusqlite_migration` (new migrations `0013_agents_extend.sql`, `0014_persona_relationships.sql`, `0015_drop_legacy_coordination.sql`), existing `Json<T>` wrapper at `crates/pattern_db/src/json_wrapper.rs` for enum columns, `smol_str::SmolStr` for ids, `jiff::Timestamp` for timestamps.

**Scope:** 6 of 7. Closes AC5 (the registry-facing parts — AC5.5, AC5.7 — left open by Phase 2), AC9 fully. Also performs the schema cleanup of the legacy coordination tables. The retirement of the staging-era types is straightforward code deletion.

**Codebase verified:** 2026-04-23.

---

## Codebase verification findings

- ✓ `agents` table at `crates/pattern_db/migrations/memory/0001_initial.sql:9-31` has: `id, name, description, model_provider, model_name, system_prompt, config, enabled_tools, tool_rules, status, created_at, updated_at`. Missing: `config_path`, `project_attachments`. Migration `0013` adds these.
- ✓ Legacy `agent_groups` + `group_members` still in `0001_initial.sql:40-61`. `group_members.capabilities` added by migration `0008`. Active queries in `crates/pattern_db/src/queries/coordination.rs` + `queries/agent.rs`. Phase 6 migrates away and drops.
- ✓ Legacy `coordination_tasks` at `0001_initial.sql:242-251` — **design plan note was stale; table is still present**. Phase 6 drops as part of `0015_drop_legacy_coordination.sql`.
- ✓ `rewrite-staging/runtime_subsystems/coordination/types.rs` contains `CoordinationPattern` enum + `AgentGroup`/`GroupMember`/`DelegationRules`/`VotingRules`/`PipelineStage`/`SleeptimeTrigger`. Not in the active workspace; delete the directory (or the coordination subtree) as part of this phase.
- ✓ `SessionConfig` / `DaemonServer` couple project attachments loosely via `project_mounts: Arc<DashMap<PathBuf, Arc<ProjectMount>>>`. Phase 6 formalizes "persona X is attached to projects [A, B]" on the persona row and persists it.
- ✗ No pre-existing "persona registry" / "agent registry" type. Greenfield work.
- ✓ `Json<T>` wrapper at `crates/pattern_db/src/json_wrapper.rs:14-73` is the established pattern for JSON-column serde. `RelationshipKind`, project-attachment lists, and group metadata all ride this.
- ✓ `PersonaId` alias lands in Phase 2 Task 1. Phase 6 uses it uniformly.
- ⚠ Phase 4 Task 4 adds an in-memory draft message queue. Phase 6 reads from it on promotion. Confirm the queue's public API (`drain_for(persona_id)`) exists before writing Task 6 code.

### Design decisions locked in

- **Flat persona registry.** One row per persona in `agents`. No hierarchical schema. Relationships live in a separate edge table — this matches the design ("flat persona registry") and lets the graph be queried independently of the persona data.
- **Groups are organisational only.** Not a coordination mechanism. `persona_groups` table holds `id`, `name`, `project_id` (optional scoping). `persona_group_members` is a simple join. Nothing in Phase 6 uses groups for dispatch; they're for human-facing organization (roster views, bulk operations).
- **Project attachments as JSON array.** `agents.project_attachments` is `JSON NOT NULL DEFAULT '[]'` — a list of project paths the persona participates in. Queries filter by array-contains via `json_each` (SQLite supports this).
- **Promotion is daemon-level RPC, not an agent effect.** Only humans can promote drafts; this is a trust boundary. Exposing it as `ctx.constellation.promote` via the Haskell surface is an anti-pattern (an agent could promote another agent). Daemon-side only.
- **Legacy-schema removal is atomic.** Migration `0015` drops `agent_groups`, `group_members`, `coordination_tasks` in one pass. Any call sites in `queries/coordination.rs` / `queries/agent.rs` are deleted in the same commit.

### Resolved

- **Legacy coordination data is disposable.** v3 is breaking the data format intentionally; `0015` does a clean `DROP TABLE` with no row-porting step. Confirmed by orual.

---

## Acceptance Criteria Coverage

### v3-multi-agent.AC5 (completion from Phase 2)

- **v3-multi-agent.AC5.5 Success:** Sibling auto-registers in the agent registry with specified relationship type
- **v3-multi-agent.AC5.7 Edge:** Draft persona appears in `ctx.constellation.list()` with `status: Draft`; calling `ctx.message.send` to a Draft persona queues the message (delivered when promoted)

### v3-multi-agent.AC9: Agent registry

- **v3-multi-agent.AC9.1 Success:** `ctx.constellation.list()` returns all personas visible in current scope with status, relationships, and group memberships
- **v3-multi-agent.AC9.2 Success:** `ctx.constellation.find(project, SupervisorOf)` returns personas with that relationship in that project
- **v3-multi-agent.AC9.3 Success:** Named group created with project scope; group visible only in that project's context
- **v3-multi-agent.AC9.4 Success:** Sibling spawn auto-registers with specified relationship; immediately visible in `ctx.constellation.list()`
- **v3-multi-agent.AC9.5 Failure:** Querying the registry for a nonexistent project returns an empty result, not an error
- **v3-multi-agent.AC9.6 Edge:** Draft personas appear in registry with `status: Draft`; they're discoverable but not steppable

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: Schema migrations — extend `agents`, add relationships + groups

**Verifies:** foundation for AC9.1-6, AC5.5, AC5.7.

**Files:**
- Create: `crates/pattern_db/migrations/memory/0013_agents_extend.sql`
- Create: `crates/pattern_db/migrations/memory/0014_persona_relationships.sql`
- Modify: `crates/pattern_db/src/migrations.rs` — register both.

**Implementation:**

```sql
-- 0013_agents_extend.sql
ALTER TABLE agents ADD COLUMN config_path TEXT;
ALTER TABLE agents ADD COLUMN project_attachments TEXT NOT NULL DEFAULT '[]';
-- status column already exists; widen accepted values: 'active', 'draft', 'inactive'.
-- Enforcement via app-level enum; SQLite doesn't enforce enum constraints.
-- idx_agents_status already exists from 0001_initial.sql:34 — do NOT re-create.
```

```sql
-- 0014_persona_relationships.sql
CREATE TABLE persona_relationships (
    id TEXT PRIMARY KEY,
    from_persona TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    to_persona   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,               -- RelationshipKind (snake_case)
    metadata     TEXT NOT NULL DEFAULT '{}',  -- Json<serde_json::Value>
    created_at   TEXT NOT NULL,               -- jiff::Timestamp RFC3339
    UNIQUE(from_persona, to_persona, kind)
);

CREATE INDEX idx_persona_relationships_from ON persona_relationships(from_persona, kind);
CREATE INDEX idx_persona_relationships_to   ON persona_relationships(to_persona, kind);

CREATE TABLE persona_groups (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    project_id TEXT,                          -- nullable (global groups allowed)
    metadata   TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    UNIQUE(name, project_id)
);

CREATE TABLE persona_group_members (
    group_id  TEXT NOT NULL REFERENCES persona_groups(id) ON DELETE CASCADE,
    persona_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    joined_at TEXT NOT NULL,
    PRIMARY KEY (group_id, persona_id)
);

CREATE INDEX idx_persona_group_members_persona ON persona_group_members(persona_id);
```

**Testing:**
- Unit (against in-memory DB): migrations run cleanly in order; tables present; indexes present.
- Unit: `UNIQUE(from_persona, to_persona, kind)` rejects duplicate edges.
- Unit: cascade delete — removing a persona drops its relationships + group memberships.

**Verification:**
`cargo nextest run -p pattern-db migrations`

**Commit:** `[pattern-db] migration 0013 + 0014: extend agents, add persona relationships + groups`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Retire legacy coordination schema

**Verifies:** none directly (cleanup); prevents regressions.

**Files:**
- Create: `crates/pattern_db/migrations/memory/0015_drop_legacy_coordination.sql`
- Delete or gut: `crates/pattern_db/src/queries/coordination.rs`
- Delete or gut: the `agent_groups` / `group_members` / `coordination_tasks` queries inside `crates/pattern_db/src/queries/agent.rs`
- Delete: `rewrite-staging/runtime_subsystems/coordination/` (entire subtree)
- Modify: any in-workspace crate that still references the old types or tables (grep first — `CoordinationPattern`, `AgentGroup`, `GroupMember`).

**Implementation:**

```sql
-- 0015_drop_legacy_coordination.sql
DROP TABLE IF EXISTS coordination_tasks;
DROP TABLE IF EXISTS group_members;
DROP TABLE IF EXISTS agent_groups;
```

Legacy rows are dropped outright — v3 is intentionally breaking the data format.

Code cleanup: grep for `CoordinationPattern`, `agent_groups`, `group_members`, `coordination_tasks`, `DelegationRules`, `VotingRules`, `PipelineStage`, `SleeptimeTrigger`. Delete every reference. Remove imports. Run `cargo check --workspace` — every call site must be updated or the code deleted.

Per `.orual/implementation-plan-guidance.md`: no backwards-compat hacks, no commented-out code, no re-exports of removed types. Delete completely.

**Testing:**
- `cargo nextest run --workspace` — full suite green after the cleanup.
- No new tests needed (this is pure deletion).

**Verification:**
`cargo nextest run --workspace && rg -F 'CoordinationPattern|agent_groups|coordination_tasks' crates/`
Expected: final grep produces zero results (outside of migration files that keep the historical names for the DROP statements).

**Commit:** `[pattern-db] [meta] retire legacy coordination schema + staging types`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->

<!-- START_TASK_3 -->
### Task 3: Extend `ConstellationRegistry` with Phase 6 methods + add group types

**Verifies:** foundation for AC9.*.

**Scope:** `ConstellationRegistry` trait, `PersonaRecord`, `PersonaStatus`, `RegistryScope`, `RegistryError`, `RelationshipEdge`, `EdgeDirection`, `GroupId` **already land in Phase 5 Task 1** (hoisted there so Phase 5 tests can exercise `FrontingResolver::resolve` against a registry). Phase 6 Task 3 modifies the existing file to:
- Extend the trait with the group- and relationship-CRUD methods.
- Add `PersonaGroup` and `RelationshipSpec` structs.
- Extend `RegistryError` with Phase 6-specific variants (e.g., `GroupNotFound`, `DuplicateGroup`).

**Files:**
- Modify: `crates/pattern_core/src/constellation.rs` — extend trait + add group types.
- Modify: `crates/pattern_core/src/lib.rs` — re-export new group types.

**Implementation:**

Extensions to the trait defined in Phase 5 Task 1:

```rust
// Added by Phase 6 Task 3 (extensions only — the base trait + types exist from Phase 5).
#[async_trait]
pub trait ConstellationRegistry: Send + Sync {
    // ...list, get (Phase 5)...
    async fn find(&self, project: Option<&Path>, kind: Option<RelationshipKind>) -> Result<Vec<PersonaRecord>, RegistryError>;
    async fn register(&self, record: PersonaRecord) -> Result<(), RegistryError>;
    async fn set_status(&self, id: &PersonaId, status: PersonaStatus) -> Result<(), RegistryError>;
    async fn add_relationship(&self, edge: RelationshipSpec) -> Result<(), RegistryError>;
    async fn groups(&self, scope: RegistryScope) -> Result<Vec<PersonaGroup>, RegistryError>;
    async fn create_group(&self, name: String, project_id: Option<String>) -> Result<PersonaGroup, RegistryError>;
}

// Added: PersonaGroup, RelationshipSpec structs (new in Phase 6).
// GroupId (SmolStr alias) lands in Phase 5 alongside PersonaRecord.
```

Pattern_core holds the trait; pattern_db has the rusqlite-backed impl (Task 4). Phase 5's `InMemoryConstellationRegistry` test helper implements only the Phase 5-defined methods; Phase 6 extends it to cover the new methods, staying behind the same `#[cfg(any(test, feature = "test-support"))]` gate.

**Testing:**
- Unit: `PersonaGroup` + `RelationshipSpec` serde round-trip.
- Unit: `RegistryError::GroupNotFound` / `DuplicateGroup` produce the expected miette diagnostics.
- (Phase 5 Task 1 tests already cover `PersonaRecord` / `RelationshipEdge` serde — do not duplicate here.)

**Verification:**
`cargo nextest run -p pattern-core constellation::groups`

**Commit:** `[pattern-core] extend ConstellationRegistry with groups + relationship methods`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: pattern_db impl of `ConstellationRegistry`

**Verifies:** AC9.1, AC9.2, AC9.3, AC9.5, AC9.6.

**Files:**
- Create: `crates/pattern_db/src/queries/constellation.rs`
- Modify: `crates/pattern_db/src/lib.rs` — expose `ConstellationRegistryDb`.

**Implementation:**

`ConstellationRegistryDb` holds an `Arc<ConstellationDb>` (existing DB handle type). Each method runs a query on the pool and maps rows to `PersonaRecord`.

Key queries:

- `list(scope)`:
  ```sql
  SELECT a.id, a.name, a.status, a.config_path, a.project_attachments
  FROM agents a
  WHERE :project IS NULL
     OR EXISTS (
          SELECT 1 FROM json_each(a.project_attachments) j
          WHERE j.value = :project
        )
  ```
  Uses SQLite's `json_each` (built-in JSON1 extension, enabled in the rusqlite
  `bundled` feature already used by pattern_db) to iterate the JSON array and
  match values. No custom functions required.
  Then load relationships and group memberships in batched follow-ups (avoid N+1 via `IN (...)` on the collected ids).

- `find(project, kind)`:
  ```sql
  SELECT a.id, ... FROM agents a
  JOIN persona_relationships r ON r.from_persona = a.id
  WHERE r.kind = :kind AND (:project IS NULL OR <project filter>)
  ```

- `get(id)`: SELECT by primary key; `Option<PersonaRecord>` for not-found.

- `register(record)`: INSERT into `agents` with UPSERT semantics (if a row exists with the same id, fall back to UPDATE). Also inserts any relationships carried on the record.

- `set_status`: trivial UPDATE. If `new_status == Active`, the caller (Task 6) follows up with a session open — the registry does NOT open sessions itself.

- `add_relationship`: INSERT into `persona_relationships` with `ON CONFLICT DO NOTHING` (dedup via the UNIQUE constraint).

- `groups(scope)`, `create_group`: CRUD against `persona_groups` + join.

AC9.5 (nonexistent project returns empty, not error): ensure `list(Some(nonexistent_path))` returns an empty vec without raising.

**Testing:**
- Unit (in-memory DB seeded with 3 personas + 2 relationships): `list(All)` returns all 3; `list(Project("p1"))` returns those attached to p1; `find(Some("p1"), SupervisorOf)` returns 1; `get(id)` returns Some; `get(nonexistent)` returns None; `list(Project("unknown"))` returns empty.
- Unit: `add_relationship` is idempotent.
- Unit: `create_group` with duplicate (name, project_id) returns a clear error.

**Verification:**
`cargo nextest run -p pattern-db constellation`

**Commit:** `[pattern-db] implement ConstellationRegistry backed by rusqlite`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `ctx.constellation.*` SDK surface

**Verifies:** AC9.1, AC9.2, AC9.3 via Haskell agent code.

**Files:**
- Create: `crates/pattern_runtime/src/sdk/requests/constellation.rs`
- Create: `crates/pattern_runtime/src/sdk/handlers/constellation.rs`
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs` — add `ConstellationHandler` to HList; extend `CANONICAL_EFFECT_ROW`.
- Modify: `crates/pattern_core/src/capability.rs` — add `EffectCategory::Constellation`.
- Create: `crates/pattern_runtime/haskell/Pattern/Constellation.hs`.

**Implementation:**

Haskell surface:

```haskell
list   :: Member Constellation effs => Maybe Scope -> Eff effs [PersonaRecord]
find   :: Member Constellation effs => Maybe Project -> Maybe RelationshipKind -> Eff effs [PersonaRecord]
groups :: Member Constellation effs => Maybe Scope -> Eff effs [PersonaGroup]
```

Read-only from agent code; writes (register, promote) are daemon-level RPCs so no Haskell surface for them. Capability-gated on `EffectCategory::Constellation`.

**Testing:**
- Integration: agent program calls `Constellation.list Nothing` and receives three personas matching the registry fixture.
- AC9.6: among results, at least one has `status = Draft` and the agent can observe it but cannot reach it via `Message.send` (delivery queues silently per Phase 4 AC6.5).

**Verification:**
`cargo nextest run -p pattern-runtime constellation_sdk`

**Commit:** `[pattern-runtime] expose ctx.constellation SDK surface`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->

<!-- START_TASK_6 -->
### Task 6: Auto-registration on sibling spawn + draft promotion

**Verifies:** AC5.5, AC5.7, AC9.4.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/sibling.rs` (Phase 2) — after a sibling session is opened (existing persona adoption OR new-identity happy path), call `registry.register(PersonaRecord { status: Active, .. })` and `registry.add_relationship(edge)` with the spawner's `RelationshipKind` from `SiblingConfig`.
- Modify: `crates/pattern_runtime/src/spawn/draft.rs` (Phase 2) — for the draft path (new-identity WITHOUT `SpawnNewIdentities` flag), call `registry.register(PersonaRecord { status: Draft, .. })`; no session opened.
- Create: `crates/pattern_server/src/rpc/promote.rs` — new RPC `PromoteDraft { persona_id: PersonaId } -> Result<(), PromoteError>` on the daemon.
- Modify: `crates/pattern_server/src/server.rs` — handle `PromoteDraft`: flip status to `Active`, open the session (same path as normal session open), register the session's mailbox with the agent registry (Phase 4), **drain the Phase 4 draft-message queue into the new mailbox in order**, emit `WireTurnEvent::FrontingChanged` only if the promoted persona is added to the fronting set (not automatic — user chooses).

**Implementation:**

```rust
// pattern_server/src/rpc/promote.rs
pub async fn handle_promote(daemon: &DaemonServer, persona_id: PersonaId) -> Result<(), PromoteError> {
    // 1. Look up the draft record.
    let record = daemon.registry.get(&persona_id).await?
        .ok_or(PromoteError::NotFound)?;
    if record.status != PersonaStatus::Draft {
        return Err(PromoteError::NotDraft(record.status));
    }

    // 2. Load the persona config from record.config_path.
    let persona = persona_loader::load_persona(record.config_path.as_ref().ok_or(PromoteError::MissingConfig)?)?;

    // 3. If the draft carries seed memory state from a fork-promote (Phase 3
    //    Task 7), use it as the initial MemoryCache. Otherwise the session
    //    opens with a fresh cache.
    let seed_cache = daemon.draft_registry.take_seed_cache(&persona_id);

    // 4. Open the session via the normal path, passing seed_cache if present.
    let session = daemon.open_session_with_seed(persona, seed_cache).await?;

    // 5. Register with agent registry (Phase 4) for mailbox routing.
    daemon.agent_registry.register(persona_id.clone(), session.mailbox_tx(), AgentStatus::Active);

    // 6. Drain the Phase 4 draft-message queue into the new mailbox.
    let queued = daemon.agent_registry.drain_draft_queue(&persona_id);
    for (msg, origin) in queued {
        session.mailbox_tx().send(MailboxInput::Message { msg, from: origin })
            .map_err(|_| PromoteError::MailboxClosed)?;
    }

    // 7. Update DB status.
    daemon.registry.set_status(&persona_id, PersonaStatus::Active).await?;

    Ok(())
}
```

**`take_seed_cache` + `open_session_with_seed`** — Phase 3 Task 7's `DraftPersona.seed_cache: Option<MemoryCache>` feeds the promotion path here. Add to the daemon:
- `DraftRegistry::take_seed_cache(&PersonaId) -> Option<MemoryCache>` — consumes the seed (take semantics, not clone — we can't re-use it after this call).
- `DaemonServer::open_session_with_seed(persona: PersonaSnapshot, seed: Option<MemoryCache>) -> Result<TidepoolSession, _>` — when `seed` is `Some`, build the session's `SessionContext` around the supplied cache rather than constructing a fresh one. Both fork-promote flows (lightweight → in-memory seed; persistent → seed carries the forked LoroDocs) land here with the same shape.

Test coverage:
- Fork-promote (lightweight): spawn fork, fork writes block `notes`, `fork.promote(cfg)` creates draft, `PromoteDraft` RPC opens session, agent reads `notes` in first turn and sees the fork's write.
- Fork-promote (persistent): same but over Standalone mount with jj — assert the jj workspace's bookmark is inherited and the persona's first turn sees the forked state.
- No-seed draft (non-fork path): sibling spawn without `SpawnNewIdentities` flag creates a draft with `seed_cache = None`; promotion opens a fresh session with empty memory.

Phase 4's draft queue exposes `drain_draft_queue(&PersonaId) -> Vec<(Message, MessageOrigin)>` (it's already hooked into the queueing side per Phase 4 Task 4). If the accessor name differs, align here — do not add a second drain API.

**Testing:**
- AC5.5: sibling spawn with `relationship = SupervisorOf` → registry has both the sibling and the edge; `ctx.constellation.list()` shows the new persona (AC9.4).
- AC5.7: without `SpawnNewIdentities`, sibling spawn creates a draft row; `ctx.constellation.list()` shows it with `status: Draft`; another agent sends a message to the draft → queued (Phase 4 behaviour); `PromoteDraft` RPC fires → session opens, queued message is delivered, drive_step runs with the message.
- Integration: two messages queued against the draft; after promotion, both delivered in original order.

**Verification:**
`cargo nextest run -p pattern-runtime sibling_autoregister && cargo nextest run -p pattern-server promote_draft`

**Commit:** `[pattern-runtime] [pattern-server] sibling auto-registration + draft promotion with queue drain`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: CLI/TUI surfaces for registry operations

**Verifies:** AC9 surfaces humans use (not AC-graded directly but necessary for smoke).

**Files:**
- Modify: `crates/pattern_cli/src/commands/` — add `constellation list`, `constellation promote <id>`, `constellation relate <from> <to> <kind>`, `constellation groups list/create`.
- Modify: `crates/pattern_cli/src/tui/` — TUI panel for the constellation (existing panels take a similar shape per `pattern_cli/CLAUDE.md`).

**Implementation:**
CLI commands: parse args, call the new daemon RPCs (`ListPersonas`, `PromoteDraft`, `AddRelationship`, `ListGroups`, `CreateGroup`), render to stdout. TUI: a simple list view subscribing to `FrontingChanged` + new `ConstellationChanged` events (emit from the daemon on any registry mutation). Keep the TUI code minimal — this phase is infrastructure, TUI polish is out of scope.

**Testing:**
- Integration: run the CLI against a daemon seeded with three personas; assert `constellation list` returns all three.
- Integration: `constellation promote <draft-id>` flips the status; re-running `constellation list` reflects `active`.

**Verification:**
`cargo nextest run -p pattern-cli constellation`

**Commit:** `[pattern-cli] add constellation subcommands + TUI panel`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase done-when checklist

- [ ] Migrations `0013`, `0014`, `0015` land cleanly; old tables dropped; staging types deleted.
- [ ] `ConstellationRegistry` trait in core; rusqlite impl in pattern_db.
- [ ] `ctx.constellation.{list,find,groups}` SDK surface (read-only) works via Haskell agent code.
- [ ] Sibling spawn auto-registers with relationship edges.
- [ ] Draft personas appear in `list()`; `PromoteDraft` RPC opens a session and drains the draft queue.
- [ ] CLI / TUI surfaces for list / promote / relate / groups land.
- [ ] No references to `CoordinationPattern` / `agent_groups` / `group_members` / `coordination_tasks` remain in active code or types.
- [ ] All existing tests still green.

---

## Notes for executor

- Legacy coordination data is disposable — no data migration step in `0015`.
- Staging-era types live outside the workspace; deletion is safe — confirm with a `cargo check --workspace` before committing the deletion.
- `drain_draft_queue` is Phase 4's API. Use it verbatim; do not add a second drain.
- CLI / TUI work is intentionally lean — Phase 7's smoke test verifies more, and pattern_cli polish is its own backlog.
- Commit style per project.
