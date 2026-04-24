# v3-multi-agent Phase 5: Fronting and routing

**Goal:** introduce a `FrontingSet` runtime primitive that tracks which persona(s) are "fronting" (the active interface to a human), persist it to `pattern_db` so it survives restart, dispatch incoming messages through a `RoutingTable` that can direct them to specialists by pattern, support direct `@persona-name` addressing that bypasses routing, and thread the `Caller` discriminant from Phase 4 Task 1 through effect handlers so human-originated turns short-circuit the permission/policy gate while agent-originated turns still pass through it.

**Architecture:** `FrontingSet` is constellation-scoped (not session-scoped) and lives on the daemon actor — one set per runtime instance, persisted in a new `fronting_set` + `routing_rules` table pair in pattern_db's memory database. Load at `DaemonServer::spawn_with_config`; save on mutation. The routing dispatcher sits in front of the `AgentRegistry` added in Phase 4 Task 4 — it resolves an incoming message to a `PersonaId` by: (1) stripping `@persona-name` prefix and sending direct if present, (2) evaluating routing rules in priority order, (3) falling back to the designated fallback persona. Co-fronting (multiple active personas) is a first-class case — unmatched messages fan out to every active persona if no fallback is specified. Human-as-caller uses the fronting persona's `SessionContext` wholesale; the broker's `request()` method checks `sender.is_human()` and returns a `PermissionGrant::synthesized_human()` without broadcast.

**Tech Stack:** `rusqlite_migration` 2.5 (already the DB-migration machinery; migration `0011_fronting.sql`), `knus` for any KDL fragment of fronting config (optional, see below), `postcard` for IRPC protocol (already the wire format — new `WireTurnEvent::FrontingChanged` variant), existing `DaemonServer` actor in `pattern_server`.

**Scope:** 5 of 7. Closes AC8 completely. AC8.1 (DB persistence) requires the new migration; AC8.8 (in-flight routing update) is the most subtle piece.

**Codebase verified:** 2026-04-23.

---

## Codebase verification findings

- ✓ Migration dir `crates/pattern_db/migrations/memory/` with 10 existing migrations. Pattern: `<NNNN>_<name>.sql` embedded via `include_str!` in `crates/pattern_db/src/migrations.rs`. Applied via `rusqlite_migration::Migrations::new_iter`. Phase 5 adds `0011_fronting.sql` with two tables (`fronting_set` for the active persona list, `routing_rules` for dispatch rules). Existing `agents` table (9 fields, incl. `status`) is the style to follow.
- ✓ `UserId` alias (`SmolStr`) at `crates/pattern_core/src/types/ids.rs:35`. Ready to use in `Caller::Human(UserId)`.
- ✓ `Caller` enum lands in Phase 4 Task 1. Phase 5 consumes it; no new caller wiring here.
- ✓ `DaemonServer` actor in `pattern_server/src/server.rs` spawns via `DaemonServer::spawn_with_config(SessionConfig { sdk, provider })` (called from `pattern_server/src/main.rs:67-137`). Sessions cached per-agent in `DaemonServer.sessions`; project mounts in `.project_mounts`. Add `fronting_set: RwLock<FrontingSet>` as a daemon-level field.
- ✓ `MessageOrigin { Author, Sphere }` in message.rs — existing discriminant. Phase 5 does NOT replace MessageOrigin (that's a compose/snapshot concern); it layers `Caller` on top for dispatch/permission gating.
- ✗ No `@persona-name` parsing. Introduce in Phase 5 in the message dispatch layer — a small `fn parse_direct_address(s: &str) -> Option<PersonaId>` that strips a leading `@` and treats the rest as the persona id. Supports both `@alice` (plain) and `@alice: hello there` (prefix form).
- ✗ `Message` carries no `to: Option<PersonaId>` field. Recipient is dispatch-time. Phase 5 keeps it that way; routing resolves recipient from rules, not from the Message struct.
- ✓ `WireTurnEvent` at `pattern_server/src/protocol.rs`; variants `Text`, `Thinking`, `ToolCall`, `ToolResult`, `Display`, `Stop`. Phase 4 adds `MessageSent`. Phase 5 adds `FrontingChanged { active: Vec<PersonaId>, fallback: Option<PersonaId>, rules: Vec<RoutingRuleWire> }`. `TaggedTurnEvent` wraps this for multi-agent fan-out already.
- ⚠ PermissionBroker is rebuilt per-runtime in Phase 1. Phase 5 adds the human short-circuit as a separate concern — `PermissionBroker::request(req, caller, timeout)` gains the `caller` parameter and returns `Some(PermissionGrant::synthesized_human())` immediately when `caller.is_human()`. Documented here; edit the broker alongside.
- ⚠ Draft-persona queue from Phase 4 Task 4 is a transient in-memory stash. Phase 5 does not promote drafts — that's Phase 6. Phase 5 ensures draft personas can NEVER appear as an active front (the setter rejects any `PersonaId` whose registry status is `Draft`).

### Design decisions locked in

- **FrontingSet ownership.** `DaemonServer` owns one; it is not per-session. Load in `DaemonServer::spawn_with_config`; save on `ctx.fronting.set/route/clear`.
- **Co-fronting semantics.** Multiple personas in `FrontingSet.active`. Unrouted messages default to the `fallback` persona; if no fallback, fan out to all active personas (every member receives a copy). The design says fan-out OR discrimination by rules — we support both via the `fallback` field's presence.
- **Routing-rule matcher types.** Strings + a small set of patterns: `Prefix(String)`, `Regex(String)`, `Contains(String)`, `TopicTag(String)`. Regex compiled once per rule (use the `regex` crate — check Cargo.toml; if not present, ask orual before adding; a prefix/contains-only initial shape is acceptable if regex adds a new dep).
- **In-flight routing updates (AC8.8).** Messages already in a mailbox queue use the routing they were resolved under. New messages use the new routing. Concretely: `RouterRegistry::route` is the only point where routing is evaluated; once a `MailboxInput` lands in an mpsc channel it's committed to its target. No re-routing.
- **Human short-circuit scope.** Applies to `Shell`, `File`, and any handler that today escalates to the broker. It does NOT bypass `MemoryPermission`/`memory_acl::check()` — memory ACL governs what blocks a persona can touch regardless of caller; the human still acts through the fronting persona, and the persona's identity is what the ACL sees.

### Open questions

**Q5.1.** Regex in routing rules requires the `regex` crate. Check workspace deps; if absent, **ask orual** before adding. A `prefix/contains/topic-tag` initial set is sufficient for the supervisor pattern and defers regex to a follow-up.

**Q5.2.** If the user clears the FrontingSet entirely (no active personas), what happens to incoming messages? Options: reject, queue in a runtime-level overflow inbox, fall back to a system default. Plan assumes **reject** with a clear `RouterError::NoActiveFronting` — a FrontingSet must have at least one active persona to accept messages. CLI/TUI surface this to the user.

---

## Acceptance Criteria Coverage

### v3-multi-agent.AC8: Fronting and routing

- **v3-multi-agent.AC8.1 Success:** FrontingSet persisted to pattern_db; after runtime restart, the same fronting set is loaded and routing resumes
- **v3-multi-agent.AC8.2 Success:** Incoming message matching a routing rule is delivered to the rule's target persona's mailbox
- **v3-multi-agent.AC8.3 Success:** Incoming message matching no routing rule is delivered to the fallback persona
- **v3-multi-agent.AC8.4 Success:** Direct addressing (`@persona-name` or explicit PersonaId) bypasses routing; delivered to named persona regardless of routing rules
- **v3-multi-agent.AC8.5 Success:** Co-fronting with two active personas: both receive copies of unrouted messages (or routing rules discriminate between them)
- **v3-multi-agent.AC8.6 Success:** `ctx.caller` is `Caller::Human(user_id)` for human-initiated turns and `Caller::Agent(persona_id)` for agent-initiated turns
- **v3-multi-agent.AC8.7 Success:** Human-as-caller uses fronting persona's SessionContext; all memory handles and project mount are the persona's
- **v3-multi-agent.AC8.8 Edge:** FrontingSet update while messages are in-flight: messages already queued use old routing; new messages use updated routing (no reprocessing)

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: `FrontingSet`, `RoutingTable`, `RoutingRule` data types

**Verifies:** foundation.

**Files:**
- Create: `crates/pattern_core/src/fronting.rs`
- Modify: `crates/pattern_core/src/lib.rs` — re-export.

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
    // Regex(String) — gated on Q5.1 + regex dep
}
```

`FrontingSet::resolve(&self, msg_body: &str) -> ResolveOutcome` returns:

```rust
pub enum ResolveOutcome<'a> {
    Direct(PersonaId),           // @persona prefix parsed
    Rule { rule_id: &'a str, target: &'a PersonaId },
    Fallback(&'a PersonaId),
    FanOut(&'a [PersonaId]),     // no fallback, co-fronted
    NoActiveFronting,
}
```

Evaluate: strip `@persona-id` prefix first → Direct. Else iterate rules by descending priority; first match → Rule. Else fallback if Some. Else if `active.len() >= 1` → FanOut. Else NoActiveFronting.

**Testing:**
- Unit: direct-addressing wins over matching rules.
- Unit: highest-priority matching rule wins.
- Unit: co-fronting fan-out when no fallback.
- Unit: NoActiveFronting when `active` is empty.
- proptest: serde round-trip on arbitrarily-generated `FrontingSet`s.

**Verification:**
`cargo nextest run -p pattern-core fronting`

**Commit:** `[pattern-core] add FrontingSet, RoutingTable, MessagePattern types`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: DB migration + load/save for FrontingSet

**Verifies:** AC8.1.

**Files:**
- Create: `crates/pattern_db/migrations/memory/0011_fronting.sql`
- Modify: `crates/pattern_db/src/migrations.rs` — register the new migration.
- Create: `crates/pattern_db/src/queries/fronting.rs` — CRUD queries.
- Modify: `crates/pattern_db/src/lib.rs` (or module re-export point) — expose the new query surface.

**Implementation:**

```sql
-- 0011_fronting.sql
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
- Modify: `crates/pattern_runtime/src/router.rs` — `RouterRegistry::route` consults the FrontingSet for agent-scheme messages before falling through to the scheme-based dispatcher.
- Modify: `crates/pattern_runtime/src/router/agent.rs` (Phase 4 Task 4) — use `FrontingSet::resolve()` to pick the target(s) when no explicit persona id is present in the payload.
- Create: `crates/pattern_runtime/src/fronting_dispatch.rs` — the dispatcher logic (`dispatch_to_mailboxes`).

**Implementation:**

Message-dispatch pseudocode:

```rust
pub async fn dispatch_to_mailboxes(
    registry: &AgentRegistry,
    fronting: &FrontingSet,
    sender: &Caller,
    body: &Message,
    explicit_target: Option<&str>,
) -> Result<(), RouterError> {
    if let Some(t) = explicit_target {
        // agent:<persona-id> from a message send call
        return registry.deliver(t.into(), sender, body).await;
    }
    match fronting.resolve(&body.text()) {
        ResolveOutcome::Direct(id) => registry.deliver(id, sender, body).await,
        ResolveOutcome::Rule { target, .. } => registry.deliver(target.clone(), sender, body).await,
        ResolveOutcome::Fallback(target) => registry.deliver(target.clone(), sender, body).await,
        ResolveOutcome::FanOut(ids) => {
            for id in ids {
                registry.deliver(id.clone(), sender, body).await?;
            }
            Ok(())
        }
        ResolveOutcome::NoActiveFronting => Err(RouterError::NoActiveFronting),
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

**Verification:**
`cargo nextest run -p pattern-runtime fronting_dispatch`

**Commit:** `[pattern-runtime] dispatch messages via FrontingSet with direct-addressing override`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Human-as-caller pathway + permission short-circuit

**Verifies:** AC8.6, AC8.7.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/` — each handler that escalates to the broker reads `cx.user().caller()`.
- Modify: `crates/pattern_runtime/src/permission/mod.rs` (Phase 1 Task 5's relocated broker) — `request(req, caller, timeout)` signature; human short-circuit.
- Modify: `crates/pattern_runtime/src/session.rs` — `SessionContext` gains `caller: Caller` (set per-turn; defaults to `Caller::System` for runtime-initiated work like wake conditions).
- Modify: `crates/pattern_runtime/src/agent_loop.rs` — at turn entry, set `ctx.caller` from the TurnInput's sender (human vs agent).

**Implementation:**

Caller set per-turn: `drive_step` accepts a `Caller` parameter (or reads it from `TurnInput::origin` when available); sets `ctx.caller` before dispatching the agent loop; resets after turn.

Broker short-circuit:

```rust
impl PermissionBroker {
    pub async fn request(&self, req: PermissionRequest, caller: &Caller, timeout: Duration) -> Option<PermissionGrant> {
        if caller.is_human() {
            return Some(PermissionGrant::synthesized_human(req.scope.clone()));
        }
        // existing policy + broadcast flow
    }
}
```

Human's SessionContext: when a human connects to a fronting persona, the daemon reuses that persona's SessionContext (workspace / project mount / memory handles) — no new context is built. This is the architectural claim in AC8.7; verify by checking that the daemon's `get_or_open_session(fronting_persona)` returns the cached session rather than constructing a new one for the human's turn.

**Testing:**
- AC8.6: assert `ctx.caller` is `Human(_)` when the turn originated from `SendMessage` RPC (human-initiated) and `Agent(_)` when it originated from `MessageReq::Send` (agent-initiated).
- AC8.7: human sends a message to the fronting persona; the turn's context references the persona's project mount + memory handles, not a fresh one.
- AC2.* regression: shell command that would normally gate still gates for `Caller::Agent`; does NOT gate for `Caller::Human`.

**Verification:**
`cargo nextest run -p pattern-runtime human_caller`

**Commit:** `[pattern-runtime] thread Caller through handlers; add human short-circuit in broker`
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
- Integration: agent without `FrontingControl` gets `EffectError::CapabilityDenied`.
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
- [ ] Migration `0011_fronting.sql` ships with CRUD queries in pattern_db.
- [ ] Daemon loads FrontingSet on spawn, saves on change, rolls back on save failure.
- [ ] Routing dispatcher handles rule-match, fallback, fan-out, direct addressing, NoActiveFronting.
- [ ] `Caller` threaded through handlers; broker short-circuits on `Caller::Human`.
- [ ] `ctx.fronting.{set,route,clear,current}` exposed; capability-gated; wire event emitted.
- [ ] Supervisor end-to-end test passes.
- [ ] All existing tests still green.

---

## Notes for executor

- **Resolve Q5.1 (regex dep) at kickoff.** If orual says no to `regex`, drop `MessagePattern::Regex` from Task 1 — prefix + contains + topic-tag cover the supervisor pattern.
- **Q5.2 (empty FrontingSet).** Plan assumes reject-with-error. If orual wants different behaviour, adjust before Task 4.
- **Registry.status lookup for Draft personas.** Phase 4 introduced the agent registry with status; Phase 5 reads it when validating `Fronting::set`. Don't duplicate status tracking.
- **AC8.8 in-flight.** The test must be genuine — queue a message, mutate fronting, then release the busy flag. Don't skip the concurrency shape.
- Commit style per project.
