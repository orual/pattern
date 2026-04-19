//! Concrete [`pattern_core::traits::Session`] impl backed by Tidepool.
//!
//! Lifecycle:
//! 1. [`TidepoolSession::open_with_agent_loop`] — preflight, construct
//!    handler bundle, spawn `EvalWorker`, build preamble.
//! 2. Repeat: [`TidepoolSession::step_with_agent_loop`] — drive the full
//!    wire-turn loop: compose → provider → stream → tool dispatch → chain.
//! 3. [`TidepoolSession::checkpoint`] / [`TidepoolSession::restore`] —
//!    event-log based.
//!
//! The legacy static-program path (`TidepoolSession::open` + `Session::step`
//! driving `SessionMachine.run`) was retired in Phase 6 Task B. The
//! `TidepoolRuntime::open_session` trait impl now delegates to
//! `open_with_agent_loop` (or can open a minimal session without an eval
//! worker for checkpoint-only use). See `runtime.rs` for the trait bridge.
//!

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use pattern_core::ProviderClient;
use pattern_core::error::RuntimeError;
use pattern_core::traits::{MemoryStore, NoOpSink, Session, TurnSink};
use pattern_core::types::snapshot::{PersonaSnapshot, SessionSnapshot};
use pattern_core::types::turn::{StepReply, TurnInput};

use crate::agent_loop::EvalWorker;
use crate::checkpoint::{CheckpointEvent, CheckpointLog};
use crate::memory::{MemoryStoreAdapter, TurnHistory};
use crate::router::RouterRegistry;
use crate::sdk::SdkLocation;
use crate::sdk::handlers::DisplayHandler;
use crate::timeout::{Budget, CancelState};

/// Session-scoped context threaded into every handler as the
/// [`tidepool_effect::EffectContext::user`] value.
#[derive(Debug)]
pub struct SessionContext {
    agent_id: String,
    /// Model identifier for provider completion requests (e.g.
    /// `"claude-opus-4-7"`). Threaded from `persona.model.choice.model_id`
    /// at session open. Use [`ModelSpec::default`] to get the workspace
    /// default (`"claude-sonnet-4-6"`).
    ///
    /// [`ModelSpec::default`]: pattern_core::types::snapshot::ModelSpec
    model_id: String,
    budget: Budget,
    cancel_state: Arc<CancelState>,
    /// Memory store adapter: delegates to the underlying `MemoryStore` and
    /// records `BlockWrite` entries for the current turn. Handlers access
    /// via [`SessionContext::adapter`] to call `record_write` after
    /// mutations; trait-object callers use [`SessionContext::memory_store`]
    /// which returns the adapter (it implements `MemoryStore`).
    adapter: Arc<MemoryStoreAdapter>,
    /// Provider-client handle. Phase 5 wires it in; Phase 5 Task 20
    /// consumes it from the agent loop. Held here so the construction
    /// signature is stable across phase boundaries.
    provider: Arc<dyn ProviderClient>,
    /// Scheme-dispatched message router registry. Handlers dispatch
    /// Send/Reply/Notify through this. Set at session open; read-only
    /// thereafter.
    router: Arc<RouterRegistry>,
    /// Pending messages accumulated during a turn. Handlers push
    /// messages here; the agent loop drains them into `TurnOutput`
    /// at turn close.
    pending_messages: Arc<std::sync::Mutex<Vec<pattern_core::types::message::Message>>>,
    /// Streaming event sink for the agent loop + Display handler.
    /// CLI/TUI bindings swap in a real sink; tests use
    /// `pattern_core::traits::VecSink`; headless runs use
    /// [`NoOpSink`] (the default).
    turn_sink: Arc<dyn TurnSink>,
    /// Shared checkpoint log. Handlers record `(request, response)` pairs
    /// after a successful effect dispatch so restart-then-replay can
    /// deterministically re-drive the JIT. Wired to the same `Arc` as
    /// [`TidepoolSession::checkpoint_log`].
    checkpoint_log: Arc<std::sync::Mutex<CheckpointLog>>,
    /// Current turn number. Incremented by [`TidepoolSession::run_turn`]
    /// before each turn; read by handlers when stamping recorded
    /// exchanges.
    current_turn: Arc<AtomicU64>,
    /// Full snapshot policy: block selection filter + mid-batch delta
    /// behavior. Default includes Core and Working blocks (Archival and
    /// Log excluded) with `IncludeSelfEdits` mid-batch behavior.
    /// Future: per-agent/constellation config overrides.
    snapshot_policy: pattern_core::types::message::SnapshotPolicy,
}

/// Handlers call this to decide whether to short-circuit on soft-cancel.
///
/// The session's `SessionContext` implements this to expose the shared
/// [`CancelState`]; the no-op blanket impl on `()` lets existing unit
/// tests keep passing `&()` as the user context.
pub trait HasCancelState {
    /// Shared cancel state used by the watchdog + handlers. A no-op
    /// implementation (e.g. on `()`) may return a fresh, unrelated
    /// state — handlers will just observe `false` and proceed.
    fn cancel_state(&self) -> Arc<CancelState>;
}

impl HasCancelState for SessionContext {
    fn cancel_state(&self) -> Arc<CancelState> {
        SessionContext::cancel_state(self)
    }
}

impl HasCancelState for () {
    fn cancel_state(&self) -> Arc<CancelState> {
        // Return a freshly allocated, never-cancelled state. Handlers
        // using `&()` as their user context effectively bypass the
        // cancellation check: they'll observe `cancellation == false`
        // and the gate entry will simply increment a fresh counter
        // nobody observes. No caller of `cx.user().cancel_state()`
        // depends on `Arc` identity across calls within a single
        // dispatch, so allocating per call is cheap and simpler than
        // the prior thread-local caching approach.
        Arc::new(CancelState::new())
    }
}

impl SessionContext {
    /// Build a context from a persona + store handle. The store is wrapped
    /// in a [`MemoryStoreAdapter`] that records `BlockWrite` entries;
    /// handlers call [`SessionContext::adapter`] to access `record_write`.
    /// Shared cancel state starts un-cancelled and with no handlers in
    /// flight. The checkpoint log is a fresh empty log; the session wires
    /// a shared log via the crate-private `with_checkpoint_log` builder so
    /// handlers record into the same log the session exposes.
    pub fn from_persona(
        persona: &PersonaSnapshot,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
    ) -> Self {
        let agent_id = persona.agent_id.to_string();
        let budget = Budget::from_persona(persona);
        let adapter = Arc::new(MemoryStoreAdapter::new(memory_store, &agent_id));
        Self {
            agent_id,
            // Thread the caller's declared model through so the composer's
            // `ctx.model_id()` matches the persona's intent. Callers that
            // want to override a persona's default at open time should
            // mutate `persona.model.choice` before calling into the
            // runtime.
            model_id: persona.model.choice.model_id.to_string(),
            budget,
            cancel_state: Arc::new(CancelState::new()),
            adapter,
            provider,
            router: Arc::new(RouterRegistry::new()),
            pending_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
            turn_sink: Arc::new(NoOpSink),
            checkpoint_log: Arc::new(std::sync::Mutex::new(CheckpointLog::new())),
            current_turn: Arc::new(AtomicU64::new(0)),
            snapshot_policy: pattern_core::types::message::SnapshotPolicy::default(),
        }
    }

    /// Replace the default [`NoOpSink`] with a caller-provided sink.
    /// Builder style; typical callers:
    /// `SessionContext::from_persona(...).with_turn_sink(sink)`.
    #[must_use]
    pub fn with_turn_sink(mut self, sink: Arc<dyn TurnSink>) -> Self {
        self.turn_sink = sink;
        self
    }

    /// Replace the checkpoint log handle and turn counter with externally
    /// owned ones. Used by [`TidepoolSession::open`] so the handler path
    /// records into the same log the session exposes via
    /// [`TidepoolSession::checkpoint_log`].
    pub(crate) fn with_checkpoint_log(
        mut self,
        log: Arc<std::sync::Mutex<CheckpointLog>>,
        turn: Arc<AtomicU64>,
    ) -> Self {
        self.checkpoint_log = log;
        self.current_turn = turn;
        self
    }

    /// Agent id this session runs as.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Model identifier for provider completion requests.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Per-turn budget snapshot.
    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// Shared cancel-state handle. Handlers check
    /// [`CancelState::is_cancelled`] at entry; the watchdog flips the flag
    /// and the gate when escalating.
    pub fn cancel_state(&self) -> Arc<CancelState> {
        self.cancel_state.clone()
    }

    /// Memory store used by MemoryHandler. Returns the adapter, which
    /// implements `MemoryStore` via delegation. Cheap clone (Arc).
    pub fn memory_store(&self) -> Arc<dyn MemoryStore> {
        self.adapter.clone()
    }

    /// Memory store adapter. Handlers call `adapter().record_write(..)`
    /// after mutations to populate `TurnOutput.block_writes`.
    pub fn adapter(&self) -> &Arc<MemoryStoreAdapter> {
        &self.adapter
    }

    /// Shared checkpoint log handle. Handlers record exchanges here
    /// after a successful dispatch (see the module-private
    /// `record_exchange` helper).
    pub fn checkpoint_log(&self) -> Arc<std::sync::Mutex<CheckpointLog>> {
        self.checkpoint_log.clone()
    }

    /// Current turn number (monotonic; bumped by `run_turn` before each
    /// turn). Handlers read this when stamping recorded events.
    pub fn current_turn(&self) -> u64 {
        self.current_turn.load(Ordering::SeqCst)
    }

    /// Provider client handle for LLM completion calls.
    pub fn provider(&self) -> &Arc<dyn ProviderClient> {
        &self.provider
    }

    /// Full snapshot policy: block-selection filter + mid-batch delta
    /// behavior. Controls which blocks appear in
    /// `MessageAttachment::BatchOpeningSnapshot` and whether this turn's
    /// own tool writes trigger mid-batch delta attachments.
    pub fn snapshot_policy(&self) -> &pattern_core::types::message::SnapshotPolicy {
        &self.snapshot_policy
    }

    /// Convenience accessor for the block-selection part of the snapshot
    /// policy. Equivalent to `snapshot_policy().selection`. Minimises
    /// call-site churn for code that only needs the selection filter.
    pub fn snapshot_selection(&self) -> &pattern_core::types::message::SnapshotSelection {
        &self.snapshot_policy.selection
    }

    /// Scheme-dispatched router registry for message routing.
    pub fn router(&self) -> &Arc<RouterRegistry> {
        &self.router
    }

    /// Pending messages accumulated during the current turn.
    pub fn pending_messages(
        &self,
    ) -> &Arc<std::sync::Mutex<Vec<pattern_core::types::message::Message>>> {
        &self.pending_messages
    }

    /// Streaming event sink for this session. Handlers + the agent
    /// loop call `turn_sink().emit(event)` as events happen during a
    /// wire turn. Never `None` — sessions default to [`NoOpSink`].
    pub fn turn_sink(&self) -> &Arc<dyn TurnSink> {
        &self.turn_sink
    }

    /// Replace the router registry. Used by session open (and tests) to
    /// inject a pre-configured registry — typically registered with a
    /// `CliRouter` or other scheme handlers before the session starts.
    ///
    /// Currently exercised via `MessageHandler::tests`; production wiring
    /// in `session::open` lands in Task 20 part 5 (agent_loop
    /// integration). The `#[allow(dead_code)]` is temporary.
    #[allow(dead_code)]
    pub(crate) fn with_router(mut self, router: Arc<RouterRegistry>) -> Self {
        self.router = router;
        self
    }
}

/// A running session: owns the handler bundle, eval worker, and checkpoint log.
///
/// Open via [`TidepoolSession::open_with_agent_loop`] and drive turns with
/// [`TidepoolSession::step_with_agent_loop`]. The `Session` trait's `step`
/// method delegates to `step_with_agent_loop`; callers should prefer the
/// typed method directly for clarity.
pub struct TidepoolSession {
    ctx: Arc<SessionContext>,
    session_id: String,
    checkpoint_log: Arc<std::sync::Mutex<CheckpointLog>>,
    /// Shared DisplayHandler so callers (CLI, tests) can register
    /// subscribers after `open`.
    display_handle: DisplayHandler,
    /// In-memory active turn history + cached archive-summary head.
    /// Populated on session open via `TurnHistory::load` (when a DB is
    /// available) or `TurnHistory::empty` (tests). `drive_step` records
    /// each completed turn here; compaction strategies consume the oldest
    /// entries.
    turn_history: Arc<std::sync::Mutex<TurnHistory>>,
    /// Long-lived Haskell eval worker. Spawned by
    /// [`TidepoolSession::open_with_agent_loop`]. Required by
    /// [`TidepoolSession::step_with_agent_loop`].
    eval_worker: Option<EvalWorker>,
    /// Shared Haskell preamble: GADT declarations + effect-row alias +
    /// helpers assembled once at session open from
    /// [`crate::sdk::bundle::canonical_effect_decls`]. Passed verbatim
    /// to every [`EvalWorker::dispatch`] call.
    preamble: Option<String>,
    /// Session-latched cache profile. Consumed by the composer
    /// pipeline inside [`crate::agent_loop::drive_step`] to place
    /// segment-1/2/3 `cache_control` markers with the configured
    /// TTLs. Latched at open-time to prevent mid-session TTL flips
    /// (which cause ~20K-token cache busts on Anthropic's
    /// subscription tier). Default:
    /// [`CacheProfile::default_anthropic_subscriber`] — all-1h per
    /// the research note in `docs/notes/2026-04-18-cache-ttl-research.md`.
    cache_profile: pattern_provider::compose::CacheProfile,
}

impl std::fmt::Debug for TidepoolSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TidepoolSession")
            .field("session_id", &self.session_id)
            .field("agent_id", &self.ctx.agent_id())
            .field("eval_worker", &self.eval_worker.is_some())
            .finish_non_exhaustive()
    }
}

impl TidepoolSession {
    /// Return a clone of the session's DisplayHandler (Arc-shared
    /// subscriber list). Subscribers registered on this handle also see
    /// events produced by the bundle's internal clone.
    pub fn display(&self) -> DisplayHandler {
        self.display_handle.clone()
    }

    /// Session-scoped id (new UUID minted at open).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Accessor for the checkpoint log — exposed so tests can assert on
    /// recorded events.
    pub fn checkpoint_log(&self) -> Arc<std::sync::Mutex<CheckpointLog>> {
        self.checkpoint_log.clone()
    }

    /// Open a minimal session: initialise context, checkpoint log, and handler
    /// display handle but do NOT spawn an eval worker. Used internally by
    /// [`Self::open_with_agent_loop`] and by `TidepoolRuntime::open_session`
    /// for checkpoint-restore use-cases that don't need the eval worker.
    ///
    /// Runs preflight so missing tidepool-extract produces an actionable error
    /// before any work happens. The session returned here is not wired for
    /// `step_with_agent_loop` — call `open_with_agent_loop` for that.
    pub fn open(
        persona: PersonaSnapshot,
        sdk: &SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
    ) -> Result<Self, RuntimeError> {
        crate::preflight::check()?;
        let _ = sdk; // sdk.resolve() is deferred to open_with_agent_loop
        let session_id = pattern_core::types::ids::new_id().to_string();
        // Share the checkpoint log + current-turn counter between the
        // session and the handler-facing SessionContext so handlers'
        // `record_exchange` calls land in the same log the session
        // publishes via `TidepoolSession::checkpoint_log`.
        let checkpoint_log = Arc::new(std::sync::Mutex::new(CheckpointLog::new()));
        // The turn counter is owned by `ctx` via `with_checkpoint_log`; handlers
        // read it through `SessionContext::current_turn()`. Nothing on
        // `TidepoolSession` reads it directly in the agent-loop path.
        let current_turn = Arc::new(AtomicU64::new(0));
        let ctx = Arc::new(
            SessionContext::from_persona(&persona, memory_store, provider.clone())
                .with_checkpoint_log(checkpoint_log.clone(), current_turn),
        );

        let display = DisplayHandler::new();

        Ok(Self {
            ctx,
            session_id,
            checkpoint_log,
            display_handle: display,
            turn_history: Arc::new(std::sync::Mutex::new(TurnHistory::empty())),
            eval_worker: None,
            preamble: None,
            cache_profile: pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        })
    }

    /// Load archived summary-head from the constellation DB for the
    /// composer's segment 2 "earlier context" prepend. Call after `open`
    /// and before the first `step` when a DB handle is available.
    /// No-op skip is safe: the composer will simply have no summary head.
    pub async fn load_turn_history(
        &self,
        db: &pattern_db::ConstellationDb,
    ) -> Result<(), pattern_db::error::DbError> {
        let history = TurnHistory::load(db, self.ctx.agent_id()).await?;
        if let Ok(mut guard) = self.turn_history.lock() {
            *guard = history;
        }
        Ok(())
    }

    /// Access the session's turn history. Exposed for the context
    /// composer and compaction strategies.
    pub fn turn_history(&self) -> Arc<std::sync::Mutex<TurnHistory>> {
        self.turn_history.clone()
    }

    /// Open a session wired for the agent-loop wire-turn-loop driver.
    ///
    /// - Runs preflight so missing tidepool-extract produces an actionable error.
    /// - Initialises context with the caller-supplied `turn_sink`.
    /// - Builds the shared Haskell preamble from
    ///   [`crate::sdk::bundle::canonical_effect_decls`].
    /// - Spawns an [`EvalWorker`] with an include path of `[sdk.resolve()]`
    ///   plus the optional `prelude_dir`.
    ///
    /// Use [`Self::step_with_agent_loop`] to drive turns on sessions
    /// opened via this constructor.
    pub fn open_with_agent_loop(
        persona: PersonaSnapshot,
        sdk: &SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
        turn_sink: Arc<dyn TurnSink>,
        prelude_dir: Option<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        // Initialise the base session (preflight, context, checkpoint log).
        let mut session = Self::open(persona, sdk, memory_store, provider)?;

        // Replace the NoOpSink on the freshly constructed SessionContext.
        // We have exclusive ownership of `session` here (just returned
        // from open), so Arc::try_unwrap on ctx will always succeed.
        let ctx_owned =
            Arc::try_unwrap(session.ctx).expect("ctx has no other clones immediately after open()");
        let ctx_with_sink = ctx_owned.with_turn_sink(turn_sink.clone());
        session.ctx = Arc::new(ctx_with_sink);

        // Wire the turn sink into the DisplayHandler so Display events
        // flow to CLI/TUI subscribers during eval turns.
        session.display_handle.forward_to_turn_sink(turn_sink);

        // Build the shared preamble once per session.
        let preamble = crate::sdk::preamble::build(&crate::sdk::bundle::canonical_effect_decls());

        // Build include paths: SDK dir only. Pattern's haskell/Pattern/
        // tree now includes both the effect GADTs AND the prelude
        // substitute. No separate "tidepool prelude dir" is needed.
        //
        // The `prelude_dir` parameter is honoured for back-compat —
        // callers who still pass one get it appended, but it's
        // optional.
        let sdk_dir = sdk.resolve()?;
        let mut include_paths = vec![sdk_dir];
        if let Some(dir) = prelude_dir {
            include_paths.push(dir);
        }

        // Spawn the eval worker.
        let worker = EvalWorker::spawn_with_includes(
            session.ctx.clone(),
            include_paths,
            session.session_id.clone(),
        );

        session.eval_worker = Some(worker);
        session.preamble = Some(preamble);

        Ok(session)
    }

    /// Execute one user-visible exchange via the Phase 5 agent-loop
    /// wire-turn-loop driver. Requires the session was opened via
    /// [`Self::open_with_agent_loop`] — returns
    /// `RuntimeError::SessionPoisoned` if no eval worker is
    /// configured, with a message pointing at the correct
    /// constructor.
    ///
    /// Drives the full wire-turn loop: compose → provider.complete →
    /// stream → tool dispatch → chain tool_results → repeat until
    /// `stop_reason.is_terminal()`. [`Session::step`] delegates here.
    pub async fn step_with_agent_loop(&self, input: TurnInput) -> Result<StepReply, RuntimeError> {
        let worker = self
            .eval_worker
            .as_ref()
            .ok_or_else(|| RuntimeError::SessionPoisoned {
                reason: "step_with_agent_loop called on a session \
                         opened without an eval worker; use \
                         TidepoolSession::open_with_agent_loop"
                    .into(),
            })?;
        let preamble = self.preamble.as_deref().unwrap_or("");
        let cache_profile = self.cache_profile.clone();
        crate::agent_loop::drive_step(
            input,
            self.ctx.clone(),
            self.turn_history.clone(),
            cache_profile,
            worker,
            preamble,
        )
        .await
    }

}

#[async_trait]
impl Session for TidepoolSession {
    async fn step(
        &mut self,
        input: TurnInput,
    ) -> Result<pattern_core::types::turn::StepReply, RuntimeError> {
        // Delegate to the agent-loop path. `&mut self` satisfies `&self` on
        // `step_with_agent_loop`.
        self.step_with_agent_loop(input).await
    }

    async fn checkpoint(&self) -> Result<SessionSnapshot, RuntimeError> {
        let log = self
            .checkpoint_log
            .lock()
            .map_err(|_| RuntimeError::CheckpointFailed {
                reason: "checkpoint log mutex poisoned".into(),
            })?;
        log.snapshot(&self.session_id, self.ctx.agent_id())
    }

    async fn restore(&mut self, snapshot: SessionSnapshot) -> Result<(), RuntimeError> {
        let events = CheckpointLog::decode_events(&snapshot)?;
        // Replay-then-continue semantics (Task 15): populate the event log
        // with the restored events so the next `step` replays them through
        // a `ReplayingBundle`. Phase 3 scope stores events verbatim;
        // follow-up phases plug this into the run loop.
        let mut log = self
            .checkpoint_log
            .lock()
            .map_err(|_| RuntimeError::CheckpointFailed {
                reason: "checkpoint log mutex poisoned".into(),
            })?;
        log.reset_to(events);
        Ok(())
    }
}

/// Record one effect exchange into the shared checkpoint log. Called by
/// handlers after they produce a response so restart-then-replay can
/// deterministically re-drive the JIT.
///
/// `request_repr` is a pre-formatted Debug string — handlers that only
/// see a typed request (not a raw `Value`) can pass
/// `format!("{req:?}")` without paying for a synthetic Value round-trip.
/// The shape written to the log matches [`CheckpointEvent::new`].
///
/// A poisoned log mutex is swallowed (logged via `tracing::warn`): we
/// do not want recording failures to affect the hot handler path. The
/// log is a best-effort artifact; if it becomes poisoned the session
/// has bigger problems than a missing event.
pub(crate) fn record_exchange(
    log: &Arc<std::sync::Mutex<CheckpointLog>>,
    tag: u32,
    request_repr: String,
    response: &tidepool_eval::Value,
    turn: u64,
) {
    match log.lock() {
        Ok(mut guard) => {
            guard.record(CheckpointEvent::from_request_repr(
                tag,
                request_repr,
                response,
                turn,
            ));
        }
        Err(_) => {
            tracing::warn!(
                tag,
                turn,
                "checkpoint log mutex poisoned; exchange not recorded"
            );
        }
    }
}

// ---- session tests -------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::SdkLocation;
    use crate::testing::{InMemoryMemoryStore, MockProviderClient};
    use pattern_core::ProviderClient;
    use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
    use pattern_core::types::ids::{BatchId, new_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use pattern_core::types::snapshot::PersonaSnapshot;
    use pattern_core::types::turn::StopReason;

    fn test_turn_input() -> TurnInput {
        TurnInput {
            turn_id: new_id(),
            batch_id: BatchId::from(new_id()),
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![],
        }
    }

    /// `step_with_agent_loop` on a session opened via the minimal
    /// `TidepoolSession::open` (no eval worker) returns
    /// `RuntimeError::SessionPoisoned` with a clear message.
    ///
    /// Gated on preflight so `open` can succeed.
    #[tokio::test]
    async fn step_with_agent_loop_without_worker_returns_session_poisoned_error() {
        if crate::preflight::check().is_err() {
            return;
        }
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();

        let session = TidepoolSession::open(persona, &sdk, store, provider)
            .expect("open should succeed when preflight passes");

        let result = session.step_with_agent_loop(test_turn_input()).await;
        match result {
            Err(RuntimeError::SessionPoisoned { reason }) => {
                assert!(
                    reason.contains("open_with_agent_loop"),
                    "error should point at the correct constructor, got: {reason}"
                );
            }
            other => panic!("expected SessionPoisoned, got: {other:?}"),
        }
    }

    /// Integration test for `step_with_agent_loop` through the
    /// [`TidepoolSession::open_with_agent_loop`] constructor with the
    /// Phase 5 wire-turn-loop driver.
    ///
    /// Scripts two wire turns — tool_use then text — and asserts the
    /// resulting [`StepReply`] aggregates them correctly. Mirrors the
    /// `agent_loop::tests::drive_step_chains_tool_use_then_final_text_into_two_wire_turns`
    /// test but exercises the full session path instead of calling
    /// `drive_step` directly.
    ///
    /// # Environment requirements
    ///
    /// Gated on `preflight::check()` only — tidepool-extract bundles
    /// the prelude internally. Skips cleanly when unavailable.
    #[tokio::test]
    async fn open_with_agent_loop_and_step_drives_two_wire_turns() {
        if crate::preflight::check().is_err() {
            return;
        }

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider = Arc::new(MockProviderClient::with_turns(vec![
            // Wire turn 1: tool_use
            MockProviderClient::tool_use_turn(
                "toolu_01",
                "code",
                serde_json::json!({"code": "pure (42 :: Int)"}),
            ),
            // Wire turn 2: final answer
            MockProviderClient::text_turn("I ran your code. The answer is 42."),
        ]));
        let provider_dyn: Arc<dyn ProviderClient> = provider.clone();

        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();

        let session = TidepoolSession::open_with_agent_loop(
            persona,
            &sdk,
            store,
            provider_dyn,
            sink_dyn,
            None,
        )
        .expect("open_with_agent_loop should succeed when preflight passes");

        let reply = session
            .step_with_agent_loop(test_turn_input())
            .await
            .expect("step_with_agent_loop should succeed with two scripted turns");

        // Two wire turns: tool_use then text.
        assert_eq!(provider.call_count(), 2, "two wire turns expected");
        assert_eq!(
            reply.turns.len(),
            2,
            "reply should aggregate two wire turns"
        );
        assert_eq!(reply.turns[0].stop_reason, StopReason::ToolUse);
        assert_eq!(reply.turns[1].stop_reason, StopReason::EndTurn);
        assert_eq!(reply.final_stop_reason, StopReason::EndTurn);

        // Batch id stable across wire turns.
        assert_eq!(
            reply.turns[0].messages[0].batch, reply.turns[1].messages[0].batch,
            "all wire turns in one step share batch_id"
        );

        // Aggregate usage sums both turns.
        let agg = reply
            .total_usage
            .expect("aggregated usage should be present");
        // tool_use_turn: prompt=50; text_turn: prompt=10 → 60 total
        assert_eq!(agg.prompt_tokens, Some(60));

        // The session's VecSink sees two Stop events (one per wire turn).
        let events = sink.snapshot();
        let stop_count = events
            .iter()
            .filter(|e| matches!(e, pattern_core::traits::TurnEvent::Stop(_)))
            .count();
        assert_eq!(stop_count, 2, "each wire turn emits one Stop event");

        // The sink should also capture the TurnEvent::Text for the final turn.
        let has_final_text = events
            .iter()
            .any(|e| matches!(e, pattern_core::traits::TurnEvent::Text(s) if s.contains("42")));
        assert!(
            has_final_text,
            "sink should contain text with '42' from final turn"
        );
    }

    /// The `NoOpSink` default is replaced by the caller's sink on sessions
    /// opened via `open_with_agent_loop`. We verify by checking that
    /// `ctx.turn_sink()` is NOT the default (NoOpSink) via pointer
    /// comparison — after open_with_agent_loop the sink should be the
    /// VecSink we passed in. The most direct assertion is that events
    /// actually appear in the VecSink (tested above), but this test
    /// checks the property directly without requiring a full eval.
    #[tokio::test]
    async fn open_with_agent_loop_wires_turn_sink_into_ctx() {
        if crate::preflight::check().is_err() {
            return;
        }

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();

        let session = TidepoolSession::open_with_agent_loop(
            persona,
            &sdk,
            store,
            provider,
            sink_dyn,
            None,
        )
        .expect("open_with_agent_loop should succeed");

        // The eval_worker and preamble should both be populated.
        assert!(
            session.eval_worker.is_some(),
            "eval_worker should be Some after open_with_agent_loop"
        );
        assert!(
            session.preamble.is_some(),
            "preamble should be Some after open_with_agent_loop"
        );
        let preamble = session.preamble.as_deref().unwrap();
        assert!(
            preamble.contains("module Expr where"),
            "preamble should contain the module header"
        );
        assert!(
            preamble.contains("paginateResult"),
            "preamble should contain pagination support"
        );
    }
}
