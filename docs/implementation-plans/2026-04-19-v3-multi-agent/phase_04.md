# v3-multi-agent Phase 4: Agent mailbox and wake conditions

**Goal:** let agents talk to each other (`ctx.message.send`) by giving every active session a tokio mailbox that wakes it with a `TurnInput` when a message arrives. Pin assigned tasks into the recipient's working-memory snapshot selection. Introduce `WakeCondition` (Rust primitives: `TaskTimeout`, `TaskDependencyResolved`, `BlockChanged`, `Interval`) and a `WakeReason` discriminant on `TurnInput` so the agent knows why it was woken. Define the Haskell surface for custom wake conditions (registration is capability-gated; full Haskell-condition evaluation is deferred — this phase ships the registration path and the Rust primitives).

**Architecture:** each active session gets a **mailbox task** — a tokio task owning an `mpsc::UnboundedReceiver<MailboxInput>`. The task watches the session's busy flag; when a message arrives and the session is idle, it calls into the existing `drive_step` with the message synthesized as a `TurnInput`. When busy, it queues. `MessageRouter` is extended with an agent-addressed scheme that resolves `PersonaId` → mailbox sender through an `AgentRegistry` (in-memory in Phase 4; DB-backed in Phase 6). The router's existing `blocked on Router trait fix` note (per `pattern_runtime/CLAUDE.md`) gets resolved here: `Router::route` gains a `sender: &MessageOrigin` parameter (reusing the existing four-way `Author` discriminant: `Partner(UserId) | Human | Agent(AgentId) | System`), and `WireTurnEvent::MessageSent` is added so the TUI can render sent messages with attribution. Wake conditions register on the mailbox task; timers, block subscribers, and task-index polls all funnel into the same mpsc as message deliveries, with a `WakeReason` tag so the agent can branch on origin.

**Tech Stack:** `tokio::sync::mpsc::UnboundedSender/Receiver` (precedent in `router.rs:63`), `tokio::sync::Notify` (new — for "busy-flag released" wake-ups), `std::sync::atomic::AtomicBool` for busy state, existing `pattern_memory::subscriber::CommitEvent` channel (extended with a `BlockChanged` notifier hook), `jiff::Span` for timeouts + intervals. No new external deps.

**Scope:** 4 of 7. Closes AC6 + AC7 in full; fixes the pre-existing "Router trait" stub called out in `pattern_runtime/CLAUDE.md`. Task-index query work lives here if Plan 2 has not yet landed it (see Q4.1).

**Codebase verified:** 2026-04-23.

---

## Codebase verification findings

- ✓ `RouterBridge` + `Router` trait + `RouterRegistry` at `crates/pattern_runtime/src/router.rs`. Current `Router::route(&self, target, body)` lacks sender identity — this is the documented "Router trait fix" in `pattern_runtime/CLAUDE.md` ("Open work"). Phase 4 fixes it.
- ✓ `Endpoint` trait at `crates/pattern_core/src/traits/endpoint.rs:43-55` — `name()` + `async fn deliver(Message)`. Only `CliEndpoint` implementation exists in active crates (`crates/pattern_runtime/src/router/cli.rs`). `MailboxEndpoint` is new work.
- ✓ Message handler at `crates/pattern_runtime/src/sdk/handlers/message.rs:96-107` already calls `registry.route(recipient, msg)`. The call path is live but no agent-scheme router is registered, so today agent-to-agent messages fall back to the default (cli) scheme. Phase 4 adds the missing router + registration point.
- ✓ `drive_step` entry at `crates/pattern_runtime/src/agent_loop.rs:838-845`, signature `drive_step(initial_input, ctx, turn_history, cache_profile, dispatcher, preamble) -> Result<StepReply, RuntimeError>`. Mailbox task invokes this when idle.
- ✓ `TurnInput` at `crates/pattern_core/src/types/turn.rs:79-92`: `turn_id, batch_id, origin: MessageOrigin, messages: Vec<Message>`. Phase 4 adds `wake: Option<WakeReason>`. Existing `TurnInput::continuation` constructor (lines 122-137) is unaffected; new `TurnInput::from_mailbox(msg, wake)` is added.
- ✓ `SnapshotPolicy.selection: SnapshotSelection` on `SessionContext`. Messages already carry `block_refs: Vec<BlockRef>` (see `agent_loop.rs:910-914`). Task pinning integrates by adding the assigned task's `BlockRef` to the mailbox message's `block_refs` — no schema change required, only wiring.
- ✓ Loro subscriber system at `crates/pattern_memory/src/subscriber.rs`: `SubscriberHandle` holds `loro::Subscription` + `crossbeam_channel::Sender<CommitEvent>`. `BlockChanged` wake condition piggybacks on this channel; we add a fan-out point that forwards specific block changes to registered wake handlers.
- ✓ `current_turn: Arc<AtomicU64>` on `SessionContext`. Busy detection via `turn_history.lock().map(|h| h.active_messages().next().is_some())` exists at `agent_loop.rs:856`. Phase 4 adds an explicit `is_in_turn: Arc<AtomicBool>` + `Arc<Notify>` pair on `SessionContext` for crisper mailbox-side busy detection — the existing turn_history check is unreliable as a live-state indicator (it's historical).
- ✓ `TurnEvent` enum at `crates/pattern_core/src/traits/turn_sink.rs:142+` has variants `Text`, `Thinking`, `ToolCall`, `ToolResult`, `Display`, `Stop`, `ComposedRequest`. Phase 4 adds `TurnEvent::Woken { reason: WakeReason }` for observability; `WireTurnEvent::MessageSent { recipient, body }` fills the gap the TUI doc listed as open work.
- ✓ `BlockSchema::TaskList` and the task-index query surface land as part of Plan 2 (task-skill-blocks). By the time Phase 4 executes, Plan 2 is complete — Phase 4 calls its query API directly via the `ctx.tasks.*` SDK surface, not through a new trait. No stub TaskQuery.
- ⚠ Pre-existing stub: `CliRouter` is mentioned in `pattern_runtime/CLAUDE.md` as blocked on the Router trait fix. Phase 4 unblocks it — we fix the trait AND implement `CliRouter` in the same pass. Per `.orual/implementation-plan-guidance.md`: pre-existing stubs in touched code become this phase's responsibility.

### Design decisions locked in

- **Mailbox granularity.** One mpsc per session. Bounded? Use `unbounded` to match existing `RouterBridge` (precedent). If a session gets flooded, the runtime's existing cancel/timeout machinery handles eventual cleanup.
- **Busy-flag mechanics.** `Arc<AtomicBool>` set by `drive_step` at entry, cleared at exit; `Arc<Notify>` signalled on exit. Mailbox task awaits `Notify::notified()` when busy, drains queue when released.
- **WakeReason + message coexistence.** A single `MailboxInput` enum carries either `Message(Message)` or `Wake(WakeReason)` or `TaskAssigned { task: BlockRef, from: PersonaId, message: Message }`. The mailbox task converts to `TurnInput` at delivery time.
- **Fairness between wakes and messages.** FIFO. No priority classes in Phase 4. Agents needing priority can filter at the Haskell layer.
- **Custom Haskell wake conditions.** Phase 4 ships the registration API (`Wake.register`) + capability flag `WakeConditionRegistration`, BUT the evaluator that runs the user's Haskell condition on a timer is deferred. The Haskell API accepts the program; the Rust handler stores it but logs-and-returns unless a dedicated eval worker is configured. This is the documented "deferred to when Tidepool concurrent evaluation is better understood" scope from the design.

---

## Acceptance Criteria Coverage

### v3-multi-agent.AC6: Agent mailbox and message delivery

- **v3-multi-agent.AC6.1 Success:** `ctx.message.send(persona_id, content)` delivers to target agent's mailbox; target steps with the message as TurnInput when idle
- **v3-multi-agent.AC6.2 Success:** Message sent to a busy agent (mid-turn) is queued; delivered after current turn completes
- **v3-multi-agent.AC6.3 Success:** Task assignment via delegation pins the task's BlockRef into the target agent's memory snapshot selection; agent sees the task in its context
- **v3-multi-agent.AC6.4 Failure:** `ctx.message.send` to a nonexistent PersonaId returns `RouterError::PersonaNotFound`
- **v3-multi-agent.AC6.5 Failure:** `ctx.message.send` to a Draft persona queues successfully but does not trigger a step (no session exists)
- **v3-multi-agent.AC6.6 Edge:** Rapid sequential messages to the same agent queue correctly; all delivered in order; no message loss under concurrent sends from multiple agents

### v3-multi-agent.AC7: Wake conditions

- **v3-multi-agent.AC7.1 Success:** `TaskTimeout(30s)` condition fires after 30 seconds if the agent's active task hasn't completed; agent receives TurnInput with `WakeReason::TaskTimeout`
- **v3-multi-agent.AC7.2 Success:** `BlockChanged(handle)` condition fires when the specified block is modified (by any agent); agent receives `WakeReason::BlockChanged(handle)`
- **v3-multi-agent.AC7.3 Success:** `TaskDependencyResolved(ref)` condition fires when the referenced task transitions to Completed; agent receives `WakeReason::DependencyResolved(ref)`
- **v3-multi-agent.AC7.4 Success:** `Interval(60s)` condition fires every 60 seconds; agent receives `WakeReason::Interval`
- **v3-multi-agent.AC7.5 Failure:** `ctx.wake.register` without `WakeConditionRegistration` capability returns `CapabilityError::Denied`
- **v3-multi-agent.AC7.6 Edge:** Multiple wake conditions registered; first to fire triggers the poke; remaining conditions stay registered for future evaluation
- **v3-multi-agent.AC7.7 Edge:** Wake condition fires while agent is mid-turn; wake is queued and delivered after current turn completes (same as message queuing)

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

<!-- START_TASK_1 -->
### Task 1: Fix `Router` trait to carry sender identity; add bypass helper on `MessageOrigin`

**Verifies:** prerequisite for AC6.1; resolves pre-existing stub.

**Files:**
- Modify: `crates/pattern_runtime/src/router.rs` — `Router::route` signature gains `sender: &MessageOrigin`.
- Modify: `crates/pattern_core/src/types/message.rs` — add `impl MessageOrigin { pub fn bypasses_permission_gate(&self) -> bool }` that returns `matches!(self.author, Author::Partner(_))`. Only `Partner` gets the bypass; general `Human` is subject to gating per project policy (TUI user is always Partner).
- Modify: `crates/pattern_server/src/protocol.rs` — add `WireTurnEvent::MessageSent { recipient, body, from: Author }` variant.
- Modify: `crates/pattern_runtime/src/sdk/handlers/message.rs` — pass the turn's `MessageOrigin` into `route()`; handler reads it from `cx.user()` or per-turn source (Phase 5 Task 5 threads it end-to-end).
- Implement: `crates/pattern_runtime/src/router/cli.rs` — `CliRouter` was stubbed per CLAUDE.md; finish the implementation now (consumes a channel to the daemon's event bus, emits `WireTurnEvent::MessageSent` on route).

**Implementation:**

No new enum. Reuse `MessageOrigin { author: Author, sphere: Sphere }` already at `crates/pattern_core/src/types/message.rs`. `Author` already discriminates `Partner(Partner{user_id}) | Human(...) | Agent(AgentAuthor{agent_id}) | System` — exactly the four-way split the broker needs.

```rust
// pattern_core/src/types/message.rs (extension)
impl MessageOrigin {
    /// Partner (the constellation's owner; TUI user) bypasses permission gating.
    /// Generic `Human` does NOT bypass — any non-Partner human still needs approval.
    pub fn bypasses_permission_gate(&self) -> bool {
        matches!(self.author, Author::Partner(_))
    }
}

// pattern_runtime/src/router.rs
#[async_trait]
pub trait Router: Send + Sync {
    fn scheme(&self) -> &str;
    async fn route(&self, sender: &MessageOrigin, target: &str, body: &Message) -> Result<(), RouterError>;
}
```

Thread `sender: &MessageOrigin` through `RouterBridge::route_sync`, `RouterRegistry::route`, and all existing implementations.

**Testing:**
- Unit: `MessageOrigin::bypasses_permission_gate` returns true for `Author::Partner(_)`, false for every other variant.
- Integration: existing router tests still pass with sender threaded through.
- Integration: `CliRouter::route` emits a `WireTurnEvent::MessageSent` on a registered test channel.

**Verification:**
`cargo nextest run -p pattern-runtime router && cargo nextest run -p pattern-server protocol`

**Commit:** `[pattern-core] [pattern-runtime] [pattern-server] fix Router trait with sender origin; add Partner bypass helper; implement CliRouter`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `Mailbox` type — per-session mpsc inbox + busy flag + Notify

**Verifies:** foundation for AC6.1, AC6.2, AC6.6.

**Files:**
- Create: `crates/pattern_runtime/src/mailbox.rs`
- Modify: `crates/pattern_runtime/src/session.rs` — `SessionContext` gains `mailbox: Arc<Mailbox>` and `is_in_turn: Arc<AtomicBool>` + `turn_done: Arc<Notify>`.
- Modify: `crates/pattern_runtime/src/agent_loop.rs` — `drive_step` sets `is_in_turn.store(true)` at entry, clears + `notify_one()` at exit (both success and error paths).

**Implementation:**

```rust
// mailbox.rs
pub enum MailboxInput {
    Message { msg: Message, from: MessageOrigin },
    TaskAssigned { task: BlockRef, from: PersonaId, msg: Message },
    Wake { reason: WakeReason },
}

pub struct Mailbox {
    tx: mpsc::UnboundedSender<MailboxInput>,
    rx: Mutex<mpsc::UnboundedReceiver<MailboxInput>>, // mutex because task pops; sender clonable
    persona_id: PersonaId,
}

impl Mailbox {
    pub fn new(persona_id: PersonaId) -> (Arc<Self>, mpsc::UnboundedSender<MailboxInput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mbx = Arc::new(Self { tx: tx.clone(), rx: Mutex::new(rx), persona_id });
        (mbx, tx)
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<MailboxInput> { self.tx.clone() }
}
```

`drive_step` wrapping (agent_loop.rs):

```rust
ctx.is_in_turn().store(true, Ordering::SeqCst);
let result = /* existing drive_step body */;
ctx.is_in_turn().store(false, Ordering::SeqCst);
ctx.turn_done().notify_one();
result
```

Even on panic, use a `defer`-style guard (or `scopeguard` crate if already a dep; else a manual `Drop` struct) to guarantee the flag clears — otherwise a panicking turn leaves the mailbox permanently blocked. Check existing deps before adding `scopeguard`; if absent, ask orual or write a tiny `DeferFlagReset` inline — favour the inline struct for zero deps.

**Testing:**
- Unit: `Mailbox::sender()` clones produce a working sender.
- Integration: drive_step on a no-op session sets and clears `is_in_turn` deterministically.
- Integration: panic in drive_step still clears `is_in_turn` (simulate via a test provider that panics).

**Verification:**
`cargo nextest run -p pattern-runtime mailbox`

**Commit:** `[pattern-runtime] add Mailbox type + busy flag around drive_step`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `MailboxTask` — the tokio task that drains the inbox into `drive_step`

**Verifies:** AC6.1, AC6.2, AC6.6.

**Files:**
- Modify: `crates/pattern_runtime/src/mailbox.rs`
- Modify: `crates/pattern_runtime/src/session.rs` — spawn the mailbox task at session open; `TidepoolSession` owns a `JoinHandle<()>` for cleanup.

**Implementation:**

```rust
pub fn spawn_mailbox_task(
    ctx: Arc<SessionContext>,
    mailbox: Arc<Mailbox>,
    turn_history: Arc<Mutex<TurnHistory>>,
    dispatcher: Arc<dyn EvalDispatcher>,
    preamble: Arc<str>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Wait until idle.
            while ctx.is_in_turn().load(Ordering::SeqCst) {
                ctx.turn_done().notified().await;
            }

            // Pull next input.
            let input = {
                let mut rx = mailbox.rx.lock().unwrap();
                rx.recv().await
            };
            let Some(input) = input else { break }; // channel closed → session ending

            // Convert to TurnInput.
            let turn_input = build_turn_input(input, &ctx);

            // Step. Errors are logged; mailbox task continues.
            if let Err(err) = drive_step(turn_input, ctx.clone(), turn_history.clone(),
                                         ctx.cache_profile(), dispatcher.as_ref(), &preamble).await {
                tracing::warn!("mailbox-triggered drive_step failed: {err}");
            }
        }
    })
}
```

`build_turn_input`:
- `Message { msg, from }` → `TurnInput` with `messages: vec![msg]`, `origin: from.into()`, `wake: None`.
- `TaskAssigned { task, from, msg }` → same, plus `msg.block_refs.push(task)` so snapshot selection pins the task.
- `Wake { reason }` → `TurnInput::continuation(..)` with `wake: Some(reason)` and empty messages.

Task termination: when the session closes (cancel_state fires), drop the mailbox sender → receiver closes → task exits. Guarantee cleanup by ensuring `TidepoolSession::Drop` drops the mailbox after asking the task to exit; `join` is best-effort with a short timeout.

**Testing:**
- Integration: open a session, send three messages in quick succession, assert each triggers a drive_step in order (AC6.6).
- Integration: mark the session busy manually (set `is_in_turn=true`), send a message, assert it's queued; clear busy flag + notify; assert the queued message is delivered (AC6.2).
- Integration: shut down session, assert mailbox task exits within 500ms (no thread leak).

**Verification:**
`cargo nextest run -p pattern-runtime mailbox_task -- --nocapture`

**Commit:** `[pattern-runtime] spawn mailbox task per session, drain inbox into drive_step`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->

<!-- START_TASK_4 -->
### Task 4: `AgentRegistry` (in-memory) + agent-scheme router

**Verifies:** AC6.1, AC6.4, AC6.5.

**Files:**
- Create: `crates/pattern_runtime/src/agent_registry.rs` — in-memory `AgentRegistry` with `DashMap<PersonaId, AgentEntry>`.
- Create: `crates/pattern_runtime/src/router/agent.rs` — `AgentRouter` impl of `Router` trait.
- Modify: `crates/pattern_runtime/src/router.rs` — register `AgentRouter` at runtime construction.

**Implementation:**

```rust
pub struct AgentEntry {
    pub mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
    pub status: AgentStatus, // Active | Draft | Inactive
}

/// Per-persona queue for messages sent to a Draft persona (no session open).
/// Phase 6's PromoteDraft RPC drains this when it flips the persona to Active.
type DraftQueue = Mutex<VecDeque<(Message, MessageOrigin)>>;

pub struct AgentRegistry {
    entries: DashMap<PersonaId, AgentEntry>,
    /// Queues for Draft personas. Entry keyed by PersonaId; exists only while
    /// the persona is in Draft status. Removed on promotion (drain) or on
    /// `unregister(persona_id)` if the Draft is abandoned.
    draft_queues: DashMap<PersonaId, DraftQueue>,
}

impl AgentRegistry {
    /// Register an active persona's mailbox sender.
    pub fn register(&self, id: PersonaId, tx: mpsc::UnboundedSender<MailboxInput>, status: AgentStatus);
    /// Unregister. If the persona was Draft, also drops any queued messages.
    pub fn unregister(&self, id: &PersonaId);
    /// Mailbox sender for the persona, if Active.
    pub fn sender(&self, id: &PersonaId) -> Option<mpsc::UnboundedSender<MailboxInput>>;
    /// Current status of the persona, if registered.
    pub fn status(&self, id: &PersonaId) -> Option<AgentStatus>;

    /// Append a message to a Draft persona's queue. Returns Err if the persona
    /// is not Draft (callers route to the mailbox instead).
    pub fn queue_for_draft(
        &self,
        id: &PersonaId,
        msg: Message,
        origin: MessageOrigin,
    ) -> Result<(), RouterError>;

    /// Drain all queued messages for a persona. Used by Phase 6's PromoteDraft RPC
    /// after flipping the persona to Active and opening its session. Returns
    /// messages in FIFO order (oldest first). Idempotent: second call returns empty.
    pub fn drain_draft_queue(&self, id: &PersonaId) -> Vec<(Message, MessageOrigin)>;
}

pub struct AgentRouter {
    registry: Arc<AgentRegistry>,
}

#[async_trait]
impl Router for AgentRouter {
    fn scheme(&self) -> &str { "agent" }
    async fn route(&self, sender: &MessageOrigin, target: &str, body: &Message) -> Result<(), RouterError> {
        let id = PersonaId::from(target.strip_prefix("agent:").unwrap_or(target));
        match self.registry.status(&id) {
            None => Err(RouterError::PersonaNotFound(id)),
            Some(AgentStatus::Draft) => {
                // AC6.5: queue for future promotion; log that no session exists.
                self.registry.queue_for_draft(&id, body.clone(), sender.clone())?;
                Ok(())
            }
            Some(_) => {
                let tx = self.registry.sender(&id).ok_or(RouterError::PersonaNotFound(id))?;
                tx.send(MailboxInput::Message { msg: body.clone(), from: sender.clone() })
                    .map_err(|_| RouterError::MailboxClosed)?;
                Ok(())
            }
        }
    }
}
```

AC6.5: draft personas accept queued messages but never deliver. When Phase 6 promotes a draft, it replays the queue into the newly-opened mailbox. Phase 4 writes the queueing path; Phase 6 consumes it. Document clearly.

Session open registers `(persona_id, mailbox_tx)` with the registry. Session close unregisters (or flips status to `Inactive`, depending on Phase 6 semantics — start with `unregister`).

**Testing:**
- AC6.1: two sessions in the same runtime; session A sends to session B's persona; B's mailbox receives.
- AC6.4: send to nonexistent persona → `RouterError::PersonaNotFound`. The error must propagate cleanly through the full call chain: `AgentRouter::route` returns `RouterError::PersonaNotFound(id)` → `RouterRegistry::route` passes it through unchanged → `MessageReq::Send` handler converts it to `EffectError::Handler(format!("{ROUTER_ERROR_PREFIX}PersonaNotFound: {id}"))` using a well-known prefix constant (consistent with Phase 1 Task 15's `PERMISSION_DENIED_PREFIX` pattern; external `tidepool-effect::EffectError` stays unchanged). Tests match on the prefix + persona id fragment.
- AC6.5: session A, persona B registered as Draft; A sends to B; response is Ok but B's mailbox (empty, since no session) remains empty. Assert queued message present in registry's draft queue.
- AC6.6: 10 concurrent messages from 3 senders to same target; target receives all 10 in a well-defined order (FIFO per-sender; interleaving across senders is non-deterministic but no loss).

**Verification:**
`cargo nextest run -p pattern-runtime agent_registry`

**Commit:** `[pattern-runtime] add AgentRegistry + agent-scheme Router for agent-to-agent delivery`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Task-pinning on delegation

**Verifies:** AC6.3.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/message.rs` — when the Message handler sees a `MessageReq::Delegate { task: BlockRef, target, body }` variant (new), route as `MailboxInput::TaskAssigned` rather than `Message`.
- Modify: `crates/pattern_runtime/src/sdk/requests/message.rs` — add `Delegate` variant.
- Modify: `crates/pattern_runtime/haskell/Pattern/Message.hs` — expose `delegate :: BlockRef -> PersonaId -> Text -> Eff effs ()` (or similar).
- No new trait; reuses Plan 2's `ctx.tasks.*` query surface in Task 9 for dependency resolution.

**Implementation:**

The mailbox task's `build_turn_input` already handles `TaskAssigned` by appending the task's `BlockRef` to the message's `block_refs`. `drive_step` already reads `messages[0].block_refs` for snapshot selection (confirmed at `agent_loop.rs:910`). No further plumbing required — this task wires up the dispatch path.

`TaskDependencyResolved` (Task 9) queries Plan 2's task-index API directly — no new trait, no new query type invented here. The `ctx.tasks.*` SDK surface from Plan 2 is the single source of truth for task state.

**Testing:**
- AC6.3: parent assigns task T to child via `ctx.message.delegate(T, child_id, "please do X")`. Child's next turn's composed request includes T's block in the snapshot (assert via insta snapshot of the composed request).

**Verification:**
`cargo nextest run -p pattern-runtime message::delegate`

**Commit:** `[pattern-runtime] wire task-pinning delegation into mailbox`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-9) -->

<!-- START_TASK_6 -->
### Task 6: `WakeReason` discriminant + `TurnInput.wake` field

**Verifies:** foundation for AC7.*.

**Files:**
- Create: `crates/pattern_core/src/wake.rs`
- Modify: `crates/pattern_core/src/types/turn.rs` — add `pub wake: Option<WakeReason>` to `TurnInput`. Update all construction sites.

**Implementation:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WakeReason {
    MessageReceived, // (implicit — set to None for message deliveries; this variant is for explicit requests)
    TaskTimeout { task: BlockRef, elapsed: jiff::Span },
    TaskDependencyResolved { task: BlockRef },
    BlockChanged { block: BlockRef },
    Interval { period: jiff::Span },
    Custom { id: String }, // Haskell-registered condition fired
}
```

`TurnInput::from_wake(wake: WakeReason, session_agent: &AgentId) -> TurnInput` constructs a no-message TurnInput tagged with the reason. Agent program can branch on `input.wake` in its Haskell code — a helper in `Pattern.Turn` exposes `wakeReason :: TurnInput -> Maybe WakeReason`.

**Threading `wake` from `drive_step` to the agent program:**
1. `build_turn_input` (Task 3, `mailbox.rs`) populates `wake` from the `MailboxInput::Wake { reason }` variant; message deliveries leave it `None`.
2. `drive_step` accepts `TurnInput` with the `wake` field already populated; no signature change beyond Phase 4 Task 2's busy-flag wrapper.
3. `compose_request_for_turn` in `agent_loop.rs` serialises the Haskell-visible `TurnInput` — include `wake` so the agent's Haskell `wakeReason` helper sees it. Add a one-line entry in the existing Haskell-bridge encoder / decoder to round-trip the field.
4. Confirm round-trip with an integration test that registers an `Interval(200ms)` wake, observes the Haskell program receive `WakeReason::Interval`, and branches on it.

**Testing:**
- Unit: serde round-trip for each variant.
- Unit: `TurnInput::from_wake(Interval{ period: Span::hours(1) }, &a)` produces `wake == Some(Interval { period: 1h })`.
- Integration: end-to-end wake pipeline — the Haskell `wakeReason` helper observes the right variant for each Rust wake primitive (see Task 7-9 tests). If the composed-request encoding drops the `wake` field, this test fails.

**Verification:**
`cargo nextest run -p pattern-core wake`

**Commit:** `[pattern-core] add WakeReason + TurnInput.wake field`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Rust wake conditions — `TaskTimeout`, `Interval`

**Verifies:** AC7.1, AC7.4, AC7.6, AC7.7.

**Files:**
- Create: `crates/pattern_runtime/src/wake/mod.rs`
- Create: `crates/pattern_runtime/src/wake/rust_primitives.rs` — Timer + Interval evaluators.
- Modify: `crates/pattern_runtime/src/session.rs` — `SessionContext` gains `wake_registry: Arc<WakeRegistry>`.

**Implementation:**

```rust
pub struct WakeRegistry {
    conditions: Mutex<Vec<RegisteredCondition>>,
    mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
}

pub struct RegisteredCondition {
    id: String,
    condition: WakeCondition,
    task_handle: JoinHandle<()>,
}

pub enum WakeCondition {
    TaskTimeout { task: BlockRef, deadline: jiff::Span },
    Interval { period: jiff::Span },
    BlockChanged { block: BlockRef },
    TaskDependencyResolved { task: BlockRef },
    Custom { id: String, program: String }, // Haskell source; evaluator deferred
}
```

For `TaskTimeout(span)`: spawn a tokio task that sleeps `span`, then (if condition is still registered) sends `MailboxInput::Wake { reason: WakeReason::TaskTimeout { task, elapsed } }` through `mailbox_tx`.

For `Interval(span)`: spawn a tokio task with a loop + `tokio::time::interval`; each tick sends `Wake { reason: Interval { .. } }`. Exit when registry is dropped.

Registering the same condition twice is allowed (per design "multiple wake conditions registered; first to fire triggers the poke; remaining conditions stay registered") — for `Interval`, "fires once" is interpreted as "fires repeatedly" per the explicit design note; the wake just doesn't deregister itself on fire. For `TaskTimeout`, the condition does deregister after firing once. Document clearly.

**Testing:**
- AC7.1: register `TaskTimeout(1s)`, wait, assert mailbox receives `Wake { reason: TaskTimeout }` and drive_step triggers a turn with that wake.
- AC7.4: register `Interval(500ms)`, observe three wake-events within 2s.
- AC7.6: register two conditions (timeout + interval); both fire independently.
- AC7.7: mark session busy; fire wake; assert the wake is queued, not dropped; clear busy flag; assert the wake is delivered.

**Verification:**
`cargo nextest run -p pattern-runtime wake::rust_primitives -- --test-threads 1 -- --nocapture`

**Commit:** `[pattern-runtime] implement TaskTimeout + Interval wake conditions`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: `BlockChanged` wake — hook loro subscriber fan-out

**Verifies:** AC7.2.

**Files:**
- Modify: `crates/pattern_memory/src/subscriber.rs` — expose a `block_change_notifier` callback hook: when a `CommitEvent` affects a block, any registered notifiers for that block are invoked.
- Modify: `crates/pattern_runtime/src/wake/mod.rs` — `BlockChanged` registers a notifier; the callback forwards to the mailbox.

**Implementation:**

In `subscriber.rs`, add (or extend the worker loop with) a `block_notifiers: DashMap<String, Vec<Box<dyn Fn(&BlockRef) + Send + Sync>>>` field. The worker thread, after processing a `CommitEvent` for block `B`, iterates registered notifiers for `B` and invokes them synchronously (the callback is lightweight — just a channel send).

```rust
// Phase 4 adds
pub fn subscribe_to_block(&self, label: &str, notifier: Box<dyn Fn(&BlockRef) + Send + Sync>);
```

Wake registration wires:

```rust
let tx = mailbox_tx.clone();
let task = task_ref.clone();
subscriber.subscribe_to_block(&block.label, Box::new(move |bref| {
    let _ = tx.send(MailboxInput::Wake { reason: WakeReason::BlockChanged { block: bref.clone() } });
}));
```

**Testing:**
- AC7.2: two sessions share a block; session A writes to the block; session B (registered for `BlockChanged`) receives the wake within 250ms.

**Verification:**
`cargo nextest run -p pattern-runtime wake::block_changed`

**Commit:** `[pattern-memory] [pattern-runtime] wire BlockChanged wake via loro subscriber fan-out`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: `TaskDependencyResolved` wake + Haskell registration API

**Verifies:** AC7.3, AC7.5.

**Files:**
- Modify: `crates/pattern_runtime/src/wake/mod.rs` — `TaskDependencyResolved` polling loop backed by `TaskQuery` trait from Task 5.
- Modify: `crates/pattern_runtime/src/sdk/requests/` — new `WakeReq::Register(WakeCondition)` / `WakeReq::Unregister(String)`.
- Create: `crates/pattern_runtime/src/sdk/handlers/wake.rs` — handler; capability-gated via `CapabilityFlag::WakeConditionRegistration`.
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs` — add `WakeHandler` to `SdkBundle` HList at the **end**, AFTER `Diagnostics` (the existing convention places Diagnostics last as session-level introspection; `Wake` joins as position 15). Extend `CANONICAL_EFFECT_ROW` with `"Wake"` in the same slot. Do NOT insert Wake mid-list — agent programs encode effect positions in their `Eff '[...]` row shapes and any earlier insertion breaks compiled programs.
- Modify: `crates/pattern_core/src/capability.rs` — flip `EffectCategory::Wake` from "reserved" to in use.
- Create: `crates/pattern_runtime/haskell/Pattern/Wake.hs` — Haskell helpers.

**Implementation:**

`TaskDependencyResolved(task_ref)` uses the existing loro subscriber machinery from Phase 4 Task 8 — **no polling**. Tasks live inside `TaskList` blocks; when the block changes, we re-check the task's status. Registration:

1. Resolve the `TaskList` block that contains `task_ref` via `ctx.tasks.parent_block(task_ref) -> BlockRef` (Plan 2 API; confirm exact name at execution time and align).
2. Call `subscriber.subscribe_to_block(&parent.label, callback)` on the resolved TaskList block (same hook used by `BlockChanged`).
3. In the callback, call `ctx.tasks.get(task_ref)`; if the returned `TaskStatus` is `Completed`, send `MailboxInput::Wake { reason: WakeReason::TaskDependencyResolved { task: task_ref } }` and call `subscriber.unsubscribe(handle)` to deregister.

Wake fires within the same latency window as `BlockChanged` (typically <250ms from the write, bounded by loro subscriber delivery). Tests assert within that window using the same harness Phase 4 Task 8 uses.

If Plan 2 hasn't exposed `parent_block(task_ref)` yet, the resolution is a plain query: scan the agent's accessible `TaskList` blocks for one containing `task_ref`. This is cheap at registration time (once per `ctx.wake.register`), not per-evaluation.

Custom Haskell conditions: the handler accepts and stores the program but logs `"custom wake condition registered; evaluator deferred"` on register. No evaluator runs yet. This is consistent with the design's stated deferral.

Capability gate: handler reads `cx.user().capabilities().has_flag(WakeConditionRegistration)`; if not, returns `EffectError::CapabilityDenied`.

**Testing:**
- AC7.3: register `TaskDependencyResolved(T)`; update T's status to Completed in another session; assert wake fires within poll interval + 250ms grace.
- AC7.5: register without `WakeConditionRegistration` capability → `EffectError::CapabilityDenied`.
- Integration: Haskell agent program calls `Wake.register (Interval 60s)` — succeeds.

**Verification:**
`cargo nextest run -p pattern-runtime wake`

**Commit:** `[pattern-runtime] expose Pattern.Wake effect with capability gate; TaskDependencyResolved polling`
<!-- END_TASK_9 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase done-when checklist

- [ ] `Router::route` carries sender identity; `CliRouter` fully implemented; `WireTurnEvent::MessageSent` lands in the protocol.
- [ ] `Mailbox` + busy flag + `Notify` integrated into `SessionContext`; `drive_step` toggles busy state cleanly (panic-safe).
- [ ] Mailbox task drains inputs into `drive_step` with message queuing + FIFO ordering.
- [ ] `AgentRegistry` resolves `PersonaId` to mailbox sender; agent-scheme router registered; draft-queue path in place.
- [ ] `Message.Delegate` pins the task `BlockRef` for the recipient's snapshot.
- [ ] `WakeReason` + `TurnInput.wake` land; Rust wake conditions (TaskTimeout, Interval, BlockChanged, TaskDependencyResolved) all fire correctly.
- [ ] `Pattern.Wake` exposed with capability gate; custom-Haskell registration accepted but deferred for evaluation.
- [ ] `TaskDependencyResolved` wake queries Plan 2's `ctx.tasks.*` API directly.
- [ ] All pre-existing tests still pass; new tests cover AC6 + AC7 end-to-end.

---

## Notes for executor

- Plan 2 (task-skill-blocks) is complete by execution time — use `ctx.tasks.*` directly for AC7.3.
- The "Router trait fix" stub in `pattern_runtime/CLAUDE.md` is resolved in Task 1. Update that CLAUDE.md note when this phase lands so future sessions don't chase a ghost.
- `scopeguard` vs inline defer: prefer inline (no new dep). If the scope-guard pattern proliferates, promote to a utility — not in this phase.
- Re-run `pattern_runtime/tests/error_clarity.rs` after wake work — new error variants mean new error-clarity coverage needed.
- Commit style per project.
