# v3-multi-agent Phase 5: Fronting and routing

**Goal:** introduce a `FrontingSet` runtime primitive that tracks which persona(s) are "fronting" (the active interface to a human), persist it to `pattern_db` so it survives restart, dispatch incoming messages through a `RoutingTable` that can direct them to specialists by pattern, support direct `@persona-name` addressing that bypasses routing, and (when applicable) thread the activating `MessageOrigin` into the daemon-actor surface so partner-bound routing decisions and audit logs can attribute correctly. **Note (revised from earlier drafts):** the broker's Partner-bypass is NOT the right tool for Partner-driven turns in the autonomous-model loop — see Phase 1 Task 7 for the rationale. The dispatch origin during normal model-driven activity is `Author::Agent(self)`, so partner-bypass is inert during normal turns. This phase's responsibility is *routing and attribution*, not auto-bypassing the permission gate based on who activated the turn.

**Architecture:** `FrontingSet` is constellation-scoped (not session-scoped) and lives on the daemon actor — one set per runtime instance, persisted in a new `fronting_set` + `routing_rules` table pair in pattern_db's memory database. Load at `DaemonServer::spawn_with_config`; save on mutation. The routing dispatcher sits in front of the `AgentRegistry` added in Phase 4 Task 4 — it resolves an incoming message to a `PersonaId` by: (1) stripping `@persona-name` prefix and sending direct if present, (2) evaluating routing rules in priority order, (3) falling back to the designated fallback persona. Co-fronting (multiple active personas) is a first-class case — unmatched messages fan out to every active persona if no fallback is specified. Partner-as-caller uses the fronting persona's `SessionContext` wholesale (memory handles, project mount, etc. — AC8.7). The activating `MessageOrigin` is preserved on the `TurnInput` for audit / routing / batch-attribution purposes, but is **not** used to drive the permission-broker bypass — see the Phase 1 Task 7 design discussion. The Phase 1 broker's bypass predicate (`Author::Partner(_)`) remains correct as a *predicate*; what changes vs. early drafts of this plan is that the dispatch origin handlers see during normal autonomous activity is `Author::Agent(self)`, not the activating Partner. Partner-bypass therefore fires only from explicit direct-execution paths that intentionally set the dispatch slot to a Partner — and Phase 5 introduces no such paths.

**Tech Stack:** `rusqlite_migration` 2.5 (already the DB-migration machinery; migration `0012_fronting.sql`), `knus` for any KDL fragment of fronting config (optional, see below), `postcard` for IRPC protocol (already the wire format — new `WireTurnEvent::FrontingChanged` variant), existing `DaemonServer` actor in `pattern_server`.

**Scope:** 5 of 7. Closes AC8 completely. AC8.1 (DB persistence) requires the new migration; AC8.8 (in-flight routing update) is the most subtle piece.

**Codebase verified:** 2026-04-23.

---

## Codebase verification findings

- ✓ Migration dir `crates/pattern_db/migrations/memory/` with 10 existing migrations. Pattern: `<NNNN>_<name>.sql` embedded via `include_str!` in `crates/pattern_db/src/migrations.rs`. Applied via `rusqlite_migration::Migrations::new_iter`. Phase 5 adds `0012_fronting.sql` with two tables (`fronting_set` for the active persona list, `routing_rules` for dispatch rules). Existing `agents` table (9 fields, incl. `status`) is the style to follow.
- ✓ `UserId` alias (`SmolStr`) at `crates/pattern_core/src/types/ids.rs:35`. Used via existing `Author::Partner(Partner { user_id })` variant in `MessageOrigin`.
- ✓ **No separate `Caller` enum.** Phase 4 Task 1 plumbs `&MessageOrigin` through `Router::route`. `MessageOrigin` already exists in the codebase with the right four-way `Author` discriminant. **Revised:** handlers read `current_dispatch_origin` (Phase 1 Task 7), which is the *immediate-caller* origin, not the activating turn's origin. Under the model-driven loop the dispatch origin is `Author::Agent(self)`; partner-bypass therefore does not fire from autonomous activity even on Partner-activated turns.
- ✓ `DaemonServer` actor in `pattern_server/src/server.rs` spawns via `DaemonServer::spawn_with_config(SessionConfig { sdk, provider })` (called from `pattern_server/src/main.rs:67-137`). Sessions cached per-agent in `DaemonServer.sessions`; project mounts in `.project_mounts`. Add `fronting_set: RwLock<FrontingSet>` as a daemon-level field.
- ✓ `MessageOrigin { Author, Sphere }` at `crates/pattern_core/src/types/origin.rs:202` — existing discriminant. Phase 5 re-uses it for both attribution (compose/snapshot, unchanged) AND permission-gating dispatch. Single source of truth; no parallel `Caller` type.
- ✗ No `@persona-name` parsing. Introduce in Phase 5 in the message dispatch layer — a small `fn parse_direct_address(s: &str) -> Option<PersonaId>` that strips a leading `@` and treats the rest as the persona id. Supports both `@alice` (plain) and `@alice: hello there` (prefix form).
- ✗ `Message` carries no `to: Option<PersonaId>` field. Recipient is dispatch-time. Phase 5 keeps it that way; routing resolves recipient from rules, not from the Message struct.
- ✓ `WireTurnEvent` at `pattern_server/src/protocol.rs`; variants `Text`, `Thinking`, `ToolCall`, `ToolResult`, `Display`, `Stop`. Phase 4 adds `MessageSent`. Phase 5 adds `FrontingChanged { active: Vec<PersonaId>, fallback: Option<PersonaId>, rules: Vec<RoutingRuleWire> }`. `TaggedTurnEvent` wraps this for multi-agent fan-out already.
- ⚠ PermissionBroker is rebuilt per-runtime in Phase 1. Phase 1 Task 6 also introduces the `origin: &MessageOrigin` parameter + Partner short-circuit predicate (the predicate stays; its inputs are constrained — see Phase 1 Task 7). Phase 5 ensures the activating `MessageOrigin` reaches the daemon-actor for routing / audit purposes, but does not change the broker's bypass semantics: handlers continue to read `current_dispatch_origin` (Agent during normal turns) for the broker call.
- ⚠ Draft-persona queue from Phase 4 Task 4 is a transient in-memory stash. Phase 5 does not promote drafts — that's Phase 6. Phase 5 ensures draft personas can NEVER appear as an active front (the setter rejects any `PersonaId` whose registry status is `Draft`).

### Design decisions locked in

- **FrontingSet ownership.** `DaemonServer` owns one; it is not per-session. Load in `DaemonServer::spawn_with_config`; save on `ctx.fronting.set/route/clear`.
- **Co-fronting semantics.** Multiple personas in `FrontingSet.active`. Unrouted messages default to the `fallback` persona; if no fallback, fan out to all active personas (every member receives a copy). The design says fan-out OR discrimination by rules — we support both via the `fallback` field's presence.
- **Routing-rule matcher types.** `Prefix(String)`, `Contains(String)`, `TopicTag(String)`, `Regex(String)`. `regex` is already a workspace dep (`Cargo.toml: regex = "1"`; used by `pattern_core`, `pattern_runtime`, `pattern_discord`) — compile once per rule at load, hold `regex::Regex` inside `RoutingTable` alongside the source string for persistence.
- **In-flight routing updates (AC8.8).** Messages already in a mailbox queue use the routing they were resolved under. New messages use the new routing. Concretely: `RouterRegistry::route` is the only point where routing is evaluated; once a `MailboxInput` lands in an mpsc channel it's committed to its target. No re-routing.
- **Human short-circuit scope (deferred).** Earlier drafts framed Partner-activated turns as auto-short-circuiting the broker. That design was retracted (see Phase 1 Task 7): handler-dispatch under the autonomous model loop sees `Author::Agent(self)` as the dispatch origin, so the bypass does not fire even on Partner-activated turns. A future "direct-execution" path (admin REPL, audited sandboxed code) is the right place to wire Partner-bypass — it would explicitly set `current_dispatch_origin` to a Partner before invoking the handler. No such path lands in Phase 5. Memory ACL semantics remain unchanged: `memory_acl::check()` governs what blocks a persona can touch regardless of caller.

### Empty FrontingSet — default-persona fallback

If the user clears the FrontingSet entirely (no `active` personas, no `fallback`), dispatch falls back to a best-available default: the first `Active` persona in the registry (sorted by id for determinism). If the registry has no `Active` personas either, route to `SystemDefault` — a synthetic persona that logs the message and ack-nowledges — so human messages are never silently dropped. The CLI/TUI exposes this via a clear "no fronting configured — using default" status line.

This avoids forcing the user to manage fronting explicitly before sending the first message; power users can configure routing whenever they want, but baseline behaviour just works.

---

## Acceptance Criteria Coverage

### v3-multi-agent.AC8: Fronting and routing

- **v3-multi-agent.AC8.1 Success:** FrontingSet persisted to pattern_db; after runtime restart, the same fronting set is loaded and routing resumes
- **v3-multi-agent.AC8.2 Success:** Incoming message matching a routing rule is delivered to the rule's target persona's mailbox
- **v3-multi-agent.AC8.3 Success:** Incoming message matching no routing rule is delivered to the fallback persona
- **v3-multi-agent.AC8.4 Success:** Direct addressing (`@persona-name` or explicit PersonaId) bypasses routing; delivered to named persona regardless of routing rules
- **v3-multi-agent.AC8.5 Success:** Co-fronting with two active personas: both receive copies of unrouted messages (or routing rules discriminate between them)
- **v3-multi-agent.AC8.6 Success:** the turn's `MessageOrigin.author` is `Author::Partner(Partner{user_id})` for TUI/Partner-initiated turns, `Author::Human(...)` for non-Partner humans, `Author::Agent(AgentAuthor{agent_id})` for agent-initiated turns, `Author::System` for runtime-initiated (wake conditions, housekeeping). The activating origin is preserved on `TurnInput` for routing and audit purposes; handlers see `current_dispatch_origin` (typically `Author::Agent(self)`) for permission decisions, not the activating origin. The Partner-bypass predicate exists on `MessageOrigin` but is exercised only by direct-execution paths (none in Phase 5).
- **v3-multi-agent.AC8.7 Success:** Human-as-caller uses fronting persona's SessionContext; all memory handles and project mount are the persona's
- **v3-multi-agent.AC8.8 Edge:** FrontingSet update while messages are in-flight: messages already queued use old routing; new messages use updated routing (no reprocessing)

### Empty-fronting fallback (additional coverage beyond listed ACs)

- Empty `active` + empty `fallback` + registry has Active personas → delivers to the lowest-id Active persona (`DefaultPersona` outcome).
- Empty `active` + empty `fallback` + registry has zero Active personas → `SystemDefault` outcome; message is acked and logged; human sees a "no fronting configured" status line.

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: `FrontingSet`, `RoutingTable`, `RoutingRule` data types

**Verifies:** foundation.

**Files:**
- Create: `crates/pattern_core/src/fronting.rs` — `FrontingSet`, `RoutingTable`, `RoutingRule`, `MessagePattern`, `ResolveOutcome`, `FrontingResolver`.
- Create: `crates/pattern_core/src/constellation.rs` — `ConstellationRegistry` trait + supporting types (`PersonaRecord`, `PersonaStatus`, `RegistryScope`, `RegistryError`, `RelationshipEdge`, `EdgeDirection`, `GroupId`). These are hoisted here from Phase 6 so Phase 5 tests can exercise default-persona resolution.
- Modify: `crates/pattern_core/src/lib.rs` — re-export both modules.

**Implementation:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct FrontingSet {
    pub active: Vec<PersonaId>,
    pub fallback: Option<PersonaId>,
    pub routing: RoutingTable,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingTable {
    pub rules: Vec<RoutingRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RoutingRule {
    pub id: String,
    pub pattern: MessagePattern,
    pub target: PersonaId,
    pub priority: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MessagePattern {
    Prefix(String),
    Contains(String),
    TopicTag(String),
    Regex(String), // source stored; compiled form cached in RoutingTable at load
}
```

`RoutingTable` compiles `Regex` variants into `regex::Regex` at construction and caches them alongside the rule list, so evaluation is hot-path cheap. Invalid regex strings fail at load with a clear `FrontingLoadError::InvalidRegex { rule_id, source, inner }`.

`FrontingResolver::resolve(&self, msg_body: &str) -> ResolveOutcome` returns:

```rust
pub enum ResolveOutcome {
    Direct(PersonaId),                   // @persona prefix parsed
    Rule { rule_id: String, target: PersonaId },
    Fallback(PersonaId),
    FanOut(Vec<PersonaId>),              // no fallback, co-fronted
    DefaultPersona(PersonaId),           // fronting empty; first Active persona from registry
    SystemDefault,                       // no Active personas exist at all
}
```

Owned throughout — PersonaId is a SmolStr (cheap to clone for ≤22-byte ids, Arc-shared beyond). Consistent ownership keeps call sites simple; no `.cloned()` dances per variant.

Evaluate: strip `@persona-id` prefix first → Direct. Else iterate rules by descending priority; first match → Rule. Else fallback if Some. Else if `active.len() >= 1` → FanOut. Else consult the `ConstellationRegistry` for the first `Active` persona sorted by id → `DefaultPersona`. Else → `SystemDefault`. Messages never fail-close on fronting state.

Because resolution needs the registry for the default-persona lookup, introduce a `FrontingResolver { set: FrontingSet, registry: Arc<dyn ConstellationRegistry> }` struct that owns both. `FrontingSet` stays as pure serializable data; `FrontingResolver::resolve(&self, msg_body: &str) -> ResolveOutcome` is the operational entry point.

**Registry trait lives in Phase 5.** The `ConstellationRegistry` trait + supporting types (`PersonaRecord`, `PersonaStatus`, `RegistryScope`, `RegistryError`, `RelationshipEdge`, `EdgeDirection`) land in `pattern_core` as part of this task (see code block below) so Phase 5's default-persona tests can exercise them. Phase 6 Task 3 is a no-op for these definitions (already defined); Phase 6 Task 4 provides the `pattern_db`-backed impl, and Phase 6 may extend the trait with `groups` / `create_group` / `PersonaGroup` when it lands the group schema.

```rust
// pattern_core/src/constellation.rs (part of Phase 5 Task 1)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PersonaRecord {
    pub id: PersonaId,
    pub name: String,
    pub status: PersonaStatus,
    pub config_path: Option<PathBuf>,
    pub project_attachments: Vec<PathBuf>,
    pub relationships: Vec<RelationshipEdge>,
    pub group_memberships: Vec<GroupId>, // empty until Phase 6 lands groups
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PersonaStatus { Active, Draft, Inactive }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipEdge {
    pub other: PersonaId,
    pub kind: RelationshipKind,
    pub direction: EdgeDirection,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum EdgeDirection { Outgoing, Incoming }

// GroupId lands here (not Phase 6) so PersonaRecord compiles in Phase 5.
// Phase 6 adds the PersonaGroup struct and related CRUD; the id type is stable.
pub type GroupId = SmolStr;

pub enum RegistryScope { All, Project(PathBuf) }

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    #[error("persona not found: {0}")]
    PersonaNotFound(PersonaId),
    #[error("registry backend unavailable")]
    BackendUnavailable,
    // additional variants added by Phase 6 as the real backend lands
}

#[async_trait]
pub trait ConstellationRegistry: Send + Sync {
    async fn list(&self, scope: RegistryScope) -> Result<Vec<PersonaRecord>, RegistryError>;
    async fn get(&self, id: &PersonaId) -> Result<Option<PersonaRecord>, RegistryError>;
    // Minimal Phase 5 surface; Phase 6 extends with find, register, set_status,
    // add_relationship, groups, create_group.
}
```

**Phase 5 tests:** an `InMemoryConstellationRegistry` helper in `pattern_runtime::testing` implements the minimal trait over a `DashMap<PersonaId, PersonaRecord>`. Tests seed personas and exercise `DefaultPersona` / `SystemDefault` outcomes deterministically.

**Testing:**
- Unit: direct-addressing wins over matching rules.
- Unit: highest-priority matching rule wins.
- Unit: co-fronting fan-out when no fallback.
- Unit: empty fronting + registry with three Active personas → DefaultPersona returns the lowest-id one.
- Unit: empty fronting + registry with zero Active → SystemDefault.
- proptest: serde round-trip on arbitrarily-generated `FrontingSet`s.
- Unit: `PersonaRecord` serde round-trip — new types from the `constellation.rs` hoist get coverage here rather than deferring to Phase 6.
- Unit: `RelationshipEdge` direction preserved in serde.
- Unit: `InMemoryConstellationRegistry::list(All)` returns every seeded persona; `list(Project(p))` filters; `get(id)` returns Some/None correctly. Exercises the hoisted trait directly.

**Verification:**
`cargo nextest run -p pattern-core fronting`

**Commit:** `[pattern-core] add FrontingSet + RoutingTable + MessagePattern types; add ConstellationRegistry trait (Phase 6 extends)`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: DB migration + load/save for FrontingSet

**Verifies:** AC8.1.

**Files:**
- Create: `crates/pattern_db/migrations/memory/0012_fronting.sql`
- Modify: `crates/pattern_db/src/migrations.rs` — register the new migration.
- Create: `crates/pattern_db/src/queries/fronting.rs` — CRUD queries.
- Modify: `crates/pattern_db/src/lib.rs` (or module re-export point) — expose the new query surface.

**Implementation:**

```sql
-- 0012_fronting.sql
CREATE TABLE fronting_set (
    id TEXT PRIMARY KEY,               -- singleton row, id = "default"
    active_personas TEXT NOT NULL,     -- JSON array of PersonaId
    fallback_persona TEXT,             -- nullable PersonaId
    updated_at TEXT NOT NULL           -- jiff::Timestamp RFC3339
);

CREATE TABLE routing_rules (
    id TEXT PRIMARY KEY,
    set_id TEXT NOT NULL REFERENCES fronting_set(id) ON DELETE CASCADE,
    pattern TEXT NOT NULL,             -- JSON-serialized MessagePattern
    target_persona TEXT NOT NULL,
    priority INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_routing_rules_priority ON routing_rules(set_id, priority DESC);
```

Queries:
- `load_fronting_set(conn: &Connection) -> Result<Option<FrontingSet>, DbError>` — joins both tables, reconstructs the struct.
- `save_fronting_set(conn: &mut Connection, set: &FrontingSet) -> Result<(), DbError>` — transactional; upserts `fronting_set`, replaces `routing_rules` for that set.
- `clear_fronting_set(conn: &mut Connection) -> Result<(), DbError>` — deletes the singleton row + cascades rules.

Use `jiff::Timestamp::now().to_string()` for `updated_at` / `created_at`. Parse back via `jiff::Timestamp::parse`. The pattern is established elsewhere in pattern_db — follow it.

**Testing:**
- Unit (against a temp in-memory DB): insert a FrontingSet, reload, assert round-trip.
- Unit: save overwrites prior routing rules (not appends).
- Unit: `clear_fronting_set` removes both tables' entries.

**Verification:**
`cargo nextest run -p pattern-db fronting`

**Commit:** `[pattern-db] add fronting_set + routing_rules migration + CRUD`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->

<!-- START_TASK_3 -->
### Task 3: `DaemonServer` owns and loads the FrontingSet

**Verifies:** AC8.1.

**Files:**
- Modify: `crates/pattern_server/src/server.rs` — `DaemonServer` gains `fronting: Arc<RwLock<FrontingSet>>`; `spawn_with_config` loads from DB on start.
- Modify: `crates/pattern_server/src/main.rs` — no behaviour change; confirm DB handle is available at daemon init.

**Implementation:**

```rust
impl DaemonServer {
    pub async fn spawn_with_config(config: SessionConfig) -> Result<ActorHandle<Self>, ...> {
        // Existing setup...
        let db = open_memory_db(&config.db_path)?;
        let fronting = db_conn.interact(|c| load_fronting_set(c)).await??.unwrap_or_default();
        let daemon = DaemonServer {
            // existing fields,
            fronting: Arc::new(RwLock::new(fronting)),
            ...
        };
        // existing spawn
    }
}
```

Save-on-change: wrap `fronting` updates in a helper that takes the write lock, mutates, calls `save_fronting_set` via the DB handle, releases the lock. If save fails, revert the in-memory change and return the error — preserve consistency.

**Testing:**
- Integration: start daemon, set fronting via RPC, shut down, start daemon again, assert fronting is preserved.
- Integration: save-failure rollback — inject a DB error, assert in-memory state matches pre-save state.

**Verification:**
`cargo nextest run -p pattern-server fronting_persistence`

**Commit:** `[pattern-server] load FrontingSet at daemon spawn, save on change`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Routing-aware message dispatch

**Verifies:** AC8.2, AC8.3, AC8.4, AC8.5.

**Files:**
- Modify: `crates/pattern_runtime/src/router/agent.rs` (from Phase 4 Task 4) — `AgentRouter::route` becomes the sole entry point for all agent-scheme deliveries (from both the Message handler and human `SendMessage` RPC). When the `target` string contains an explicit `agent:<persona-id>`, route direct as Phase 4 Task 4 already does. When it's empty or contains routing sentinels like `fronting:` / `auto:`, delegate to `dispatch_to_mailboxes`.
- Create: `crates/pattern_runtime/src/fronting_dispatch.rs` — the routing-resolution function `dispatch_to_mailboxes(registry, resolver, sender, body) -> Result<(), RouterError>`. This is a pure router function called BY `AgentRouter::route`; it does not own message dispatch, just target selection. `AgentRouter::route` is the single entry point.

**Implementation:**

Two-layer responsibility:
- `AgentRouter::route` — called from any message-dispatch site (Phase 4's Message handler, Phase 5's human SendMessage path, Phase 5 Task 7's supervisor routing). Handles explicit-target (direct addressing) + draft queueing (Phase 4 Task 4's `queue_for_draft` branch). Delegates to `dispatch_to_mailboxes` when the target is unspecified.
- `dispatch_to_mailboxes` — evaluates the FrontingSet resolver, returns a list of `PersonaId`s, then calls back into the registry's delivery primitive for each. Does NOT know about the Message handler or the RPC surface.

Message-dispatch pseudocode (inside `dispatch_to_mailboxes`, called from `AgentRouter::route`):

```rust
pub async fn dispatch_to_mailboxes(
    registry: &AgentRegistry,
    fronting: &FrontingSet,
    sender: &MessageOrigin,
    body: &Message,
    explicit_target: Option<&str>,
) -> Result<(), RouterError> {
    if let Some(t) = explicit_target {
        // agent:<persona-id> from a message send call
        return registry.deliver(t.into(), sender, body).await;
    }
    match resolver.resolve(&body.text()) {
        ResolveOutcome::Direct(id) => registry.deliver(id, sender, body).await,
        ResolveOutcome::Rule { target, .. } => registry.deliver(target, sender, body).await,
        ResolveOutcome::Fallback(target) => registry.deliver(target, sender, body).await,
        ResolveOutcome::FanOut(ids) => {
            for id in ids {
                registry.deliver(id, sender, body).await?;
            }
            Ok(())
        }
        ResolveOutcome::DefaultPersona(id) => registry.deliver(id, sender, body).await,
        ResolveOutcome::SystemDefault => registry.deliver_system_default(sender, body).await,
    }
}
```

`@persona-name` parsing: happens in `Message::text()` or in the dispatcher before `resolve()`. If a prefix match fires, the dispatcher overrides `resolve()` and takes the Direct path. Snip the prefix off the message body before delivery so the recipient doesn't see `@alice`.

**Testing:**
- AC8.2: rule `{ pattern: Prefix("!math"), target: math_persona }`; message `"!math 2+2"` delivered to math_persona.
- AC8.3: no rules match; message delivered to fallback.
- AC8.4: `"@alice please do X"` delivered to alice regardless of rules.
- AC8.5: two active personas, no fallback, message with no rule match → both mailboxes receive a copy.
- AC8.8: submit a message, immediately update routing, submit a second message — first goes to old target, second to new target.
- Empty-fronting default: clear the FrontingSet entirely; send a message; assert it lands in the lowest-id Active persona's mailbox with a status event surfaced to the human.
- System default: additionally mark all personas as Inactive; send a message; assert the `SystemDefault` path acks without crashing and emits a `FrontingMissing` diagnostic.

**Verification:**
`cargo nextest run -p pattern-runtime fronting_dispatch`

**Commit:** `[pattern-runtime] dispatch messages via FrontingSet with direct-addressing override`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Thread activating `MessageOrigin` into the daemon-actor surface

**Verifies:** AC8.6, AC8.7.

**Files:**
- Modify: `crates/pattern_runtime/src/agent_loop.rs` — confirm `drive_step` continues to publish `Author::Agent(self)` to `current_dispatch_origin` per orchestrate iteration (Phase 1 Task 7 wiring; this task verifies it). The activating `TurnInput.origin` is already preserved on the input itself for batch-type inference, persistence attribution, and routing decisions; no new wiring needed for that.
- Modify: `crates/pattern_server/` — when receiving a `SendMessage` RPC from the TUI, construct the `TurnInput.origin` as `Author::Partner(...)` with the partner's `UserId`. When a `MessageReq::Send` from another agent activates a turn, the origin is `Author::Agent(...)`. The daemon already constructs `MessageOrigin` for inbound turns; this task tightens the attribution so Partner / Human / Agent / System are correctly discriminated at the source.
- Modify: `crates/pattern_runtime/src/session.rs` — no broker call-site changes here; Phase 1 Task 7 already wired `HasPermissionBridge` and the dispatch-origin slot.

**Implementation:**

`TurnInput` already carries `origin: MessageOrigin`; `drive_step` (Phase 1 Task 7) already publishes `Author::Agent(self)` to `current_dispatch_origin` per orchestrate iteration. This task's responsibility is to ensure the *activating* origin reaches the daemon correctly attributed. There is no broker bypass change in this task — the bypass predicate stays as it is, and the dispatch origin handlers see continues to be `Author::Agent(self)` during normal turns.

```rust
// pattern_server/src/server.rs — when handling SendMessage RPC:
let activating_origin = MessageOrigin::new(
    Author::Partner(Partner { user_id: partner_id.clone() }),
    Sphere::Private,                                    // or sphere from RPC context
);
let input = TurnInput {
    /* … */
    origin: activating_origin,
    /* … */
};

// Direct-execution paths (NOT in Phase 5, but the shape that would
// genuinely warrant Partner-bypass) would set the dispatch slot:
//
// {
//     let mut slot = ctx.current_dispatch_origin_slot().write().unwrap();
//     *slot = Some(MessageOrigin::new(Author::Partner(p), Sphere::Private));
// }
// // …invoke handler directly here…
```

Human's SessionContext: when a human connects to a fronting persona, the daemon reuses that persona's SessionContext (workspace / project mount / memory handles) — no new context is built. This is the architectural claim in AC8.7; verify by checking that the daemon's `get_or_open_session(fronting_persona)` returns the cached session rather than constructing a new one for the human's turn.

**Testing:**
- AC8.6: assert `TurnInput.origin.author` is `Author::Partner(_)` when the turn originated from `SendMessage` RPC (TUI user is Partner); `Author::Agent(_)` when it originated from `MessageReq::Send` from another agent. Also assert that during the turn, `ctx.current_dispatch_origin()` reports `Author::Agent(self)` regardless of the activating author — this is the security invariant that prevents Partner authority leaking into autonomous agent activity.
- AC8.7: human (Partner) sends a message to the fronting persona; the turn's context references the persona's project mount + memory handles, not a fresh one.
- AC2.* regression: shell command that would normally gate still gates for `Author::Agent`; does NOT gate for `Author::Partner`; DOES gate for `Author::Human(_)` (generic human is not Partner).

**Verification:**
`cargo nextest run -p pattern-runtime origin_short_circuit`

**Commit:** `[pattern-runtime] thread turn MessageOrigin through handlers; add Partner short-circuit in broker`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->

<!-- START_TASK_6 -->
### Task 6: SDK surface — `ctx.fronting.{set,route,current}`

**Verifies:** AC8 via Haskell agent code; RPC for CLI/TUI.

**Files:**
- Create: `crates/pattern_runtime/src/sdk/requests/fronting.rs`
- Create: `crates/pattern_runtime/src/sdk/handlers/fronting.rs`
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs` — add `FrontingHandler` to the HList; `CANONICAL_EFFECT_ROW` gains `"Fronting"`.
- Modify: `crates/pattern_core/src/capability.rs` — add `EffectCategory::Fronting`.
- Create: `crates/pattern_runtime/haskell/Pattern/Fronting.hs` — helpers.
- Modify: `crates/pattern_server/src/protocol.rs` — `FrontingGetRequest`, `FrontingSetRequest`, `FrontingChanged` wire event.

**Implementation:**

Haskell surface:

```haskell
current :: Member Fronting effs => Eff effs FrontingSnapshot
set     :: Member Fronting effs => [PersonaId] -> Maybe PersonaId -> Eff effs ()
route   :: Member Fronting effs => [RoutingRule] -> Eff effs ()
clear   :: Member Fronting effs => Eff effs ()
```

Capability-gated: setting/routing requires the `FrontingControl` flag on the persona's CapabilitySet (a new flag, added to Phase 2's `CapabilityFlag` enum). Most agents don't have it; a "supervisor" persona does.

Daemon side (pattern_server): `DaemonServer` exposes `GetFronting`, `SetFronting`, `UpdateRouting` RPCs for the TUI. These bypass the Haskell handler entirely — they're human-privileged DB writes. Emit `WireTurnEvent::FrontingChanged` to subscribers on every change.

**Testing:**
- Integration: agent with `FrontingControl` can call `Fronting.set [alice, bob] (Just alice)`; DB row updated.
- Integration: agent without `FrontingControl` gets `EffectError::Handler` whose message starts with `CAPABILITY_DENIED_PREFIX`.
- RPC: TUI client issues `SetFronting`; receives `FrontingChanged` back; subsequent message routing follows the new rules (AC8.8 — verify the in-flight case).

**Verification:**
`cargo nextest run -p pattern-runtime fronting_sdk && cargo nextest run -p pattern-server fronting_rpc`

**Commit:** `[pattern-runtime] [pattern-server] expose ctx.fronting SDK + daemon RPCs + FrontingChanged event`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Supervisor pattern end-to-end integration test

**Verifies:** AC8.2, AC8.3, AC8.7 composed.

**Files:**
- Create: `crates/pattern_runtime/tests/fronting_supervisor.rs`
- Create: `crates/pattern_runtime/tests/fixtures/supervisor_persona.kdl`
- Create: `crates/pattern_runtime/tests/fixtures/math_specialist.kdl`
- Create: `crates/pattern_runtime/tests/fixtures/chat_specialist.kdl`

**Implementation:**

Scenario:
1. Load three persona fixtures: supervisor (permanently fronting, `FrontingControl` flag), math-specialist, chat-specialist.
2. Configure `FrontingSet { active: [supervisor], fallback: Some(supervisor), routing: [{ Prefix("!math"), math-specialist, 10 }, { Contains("chat"), chat-specialist, 5 }] }`.
3. Human sends three messages: `"hello"` → supervisor (fallback), `"!math 2+2"` → math, `"lets chat"` → chat.
4. Assert each specialist's mailbox received the right message; supervisor saw `"hello"` only.
5. Restart the daemon; assert fronting state survives; repeat message delivery.

**Testing:**
- End-to-end integration using the scripted/mock provider from Phase 2's infrastructure.
- Test is runnable under `cargo nextest run` without any external setup beyond the mock.

**Verification:**
`cargo nextest run -p pattern-runtime fronting_supervisor -- --nocapture`

**Commit:** `[pattern-runtime] supervisor pattern end-to-end integration test`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase done-when checklist

- [ ] `FrontingSet` + `RoutingTable` + `RoutingRule` + `MessagePattern` types land in `pattern_core`.
- [ ] Migration `0012_fronting.sql` ships with CRUD queries in pattern_db.
- [ ] Daemon loads FrontingSet on spawn, saves on change, rolls back on save failure.
- [ ] Routing dispatcher handles rule-match, fallback, fan-out, direct addressing, empty-fronting default-persona lookup, system-default ack.
- [ ] `MessageOrigin` reachable from handlers via `EffectContext`; broker short-circuits on `origin.bypasses_permission_gate()` (Partner-only).
- [ ] `ctx.fronting.{set,route,clear,current}` exposed; capability-gated; wire event emitted.
- [ ] Supervisor end-to-end test passes.
- [ ] All existing tests still green.

---

## Notes for executor

- **Empty-fronting default.** The resolver is responsible for falling through to the registry's best-available Active persona; only when there are zero Active personas does it hand off to the system default. Message delivery never fails-closed on fronting state.
- **Registry.status lookup for Draft personas.** Phase 4 introduced the agent registry with status; Phase 5 reads it when validating `Fronting::set`. Don't duplicate status tracking.
- **AC8.8 in-flight.** The test must be genuine — queue a message, mutate fronting, then release the busy flag. Don't skip the concurrency shape.
- Commit style per project.
