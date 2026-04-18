//! Concrete [`pattern_core::traits::Session`] impl backed by Tidepool.
//!
//! Lifecycle:
//! 1. [`TidepoolSession::open`] — preflight, compile, warm JIT, construct
//!    a bundle (handlers parameterised over [`SessionContext`]).
//! 2. Repeat: [`TidepoolSession::step`] — run the JIT with a turn input
//!    threaded through effect handlers, collect turn output.
//! 3. [`TidepoolSession::checkpoint`] / [`TidepoolSession::restore`] —
//!    event-log based (Phase 3 Task 15).
//!
//! Phase 3 scope: MemoryHandler dispatches to the session's
//! `Arc<dyn MemoryStore>`; MessageHandler is stubbed; handlers
//! co-operatively check [`SessionContext::cancel_state`] at entry.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use jiff::Timestamp;
use pattern_core::ProviderClient;
use pattern_core::error::{CancelPath, RuntimeError};
use pattern_core::traits::{MemoryStore, Session};
use pattern_core::types::snapshot::{PersonaConfig, SessionSnapshot};
use pattern_core::types::turn::{TurnInput, TurnOutput};

use crate::checkpoint::{CheckpointEvent, CheckpointLog};
use crate::memory::{MemoryStoreAdapter, TurnHistory};
use crate::router::RouterRegistry;
use crate::sdk::SdkLocation;
use crate::sdk::bundle::SdkBundle;
use crate::sdk::handlers::{
    DisplayHandler, FileHandler, LogHandler, McpHandler, MemoryHandler, MessageHandler, RpcHandler,
    ShellHandler, SourcesHandler, SpawnHandler, TimeHandler,
};
use crate::tidepool::{CancelHandle, SessionMachine, compile_program};
use crate::timeout::{Budget, CancelState};

/// Session-scoped context threaded into every handler as the
/// [`tidepool_effect::EffectContext::user`] value.
///
/// Phase 3 fields:
/// - [`SessionContext::agent_id`] — stable agent identifier, needed by
///   MemoryHandler to disambiguate memory blocks.
/// - [`SessionContext::budget`] — per-turn budget snapshot from PersonaConfig.
/// - [`SessionContext::cancel_state`] — shared atomic flag + handler gate
///   driving the two-path cancellation harness (Task 16).
/// - [`SessionContext::memory_store`] — `Arc<dyn MemoryStore>` that the
///   MemoryHandler dispatches reads/writes to. Trait-object dispatch is
///   deliberate: `pattern_runtime` must not compile-link to any concrete
///   memory backend (Phase 2 architecture rule).
///
#[derive(Debug)]
pub struct SessionContext {
    agent_id: String,
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
    /// Shared checkpoint log. Handlers record `(request, response)` pairs
    /// after a successful effect dispatch so restart-then-replay can
    /// deterministically re-drive the JIT. Wired to the same `Arc` as
    /// [`TidepoolSession::checkpoint_log`].
    checkpoint_log: Arc<std::sync::Mutex<CheckpointLog>>,
    /// Current turn number. Incremented by [`TidepoolSession::run_turn`]
    /// before each turn; read by handlers when stamping recorded
    /// exchanges.
    current_turn: Arc<AtomicU64>,
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
        persona: &PersonaConfig,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
    ) -> Self {
        let agent_id = persona.agent_id.to_string();
        let budget = Budget::from_persona(persona);
        let adapter = Arc::new(MemoryStoreAdapter::new(memory_store, &agent_id));
        Self {
            agent_id,
            budget,
            cancel_state: Arc::new(CancelState::new()),
            adapter,
            provider,
            router: Arc::new(RouterRegistry::new()),
            pending_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
            checkpoint_log: Arc::new(std::sync::Mutex::new(CheckpointLog::new())),
            current_turn: Arc::new(AtomicU64::new(0)),
        }
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

    /// Scheme-dispatched router registry for message routing.
    pub fn router(&self) -> &Arc<RouterRegistry> {
        &self.router
    }

    /// Pending messages accumulated during the current turn.
    pub fn pending_messages(&self) -> &Arc<std::sync::Mutex<Vec<pattern_core::types::message::Message>>> {
        &self.pending_messages
    }

    /// Replace the router registry. Used by session open to inject a
    /// pre-configured registry.
    pub(crate) fn with_router(mut self, router: Arc<RouterRegistry>) -> Self {
        self.router = router;
        self
    }
}

/// A running session: owns the JIT machine, handler bundle, cancellation
/// harness, and checkpoint log.
///
/// The machine is held inside an `Option<Box<...>>` so `step` can move it
/// into a `spawn_blocking` task (tokio requires `'static` closures) and
/// move it back on normal completion or soft cancel.
pub struct TidepoolSession {
    /// JIT machine + bundle held behind a Mutex so the session struct is
    /// `Sync`. The underlying types are `Send` but not `Sync` (their
    /// internals hold raw pointers / `RefCell`s); the mutex gives us the
    /// `Sync` bound the async `Session` trait's `&self`-returning
    /// futures require.
    inner: std::sync::Mutex<InnerState>,
    ctx: Arc<SessionContext>,
    session_id: String,
    checkpoint_log: Arc<std::sync::Mutex<CheckpointLog>>,
    /// Monotonic turn counter exposed to handlers via SessionContext so
    /// recorded exchanges can be stamped with the current turn. Shared
    /// `Arc<AtomicU64>` with `ctx.current_turn`.
    current_turn: Arc<AtomicU64>,
    /// Shared DisplayHandler so callers (CLI, tests) can register
    /// subscribers after `open`.
    display_handle: DisplayHandler,
    /// External cancel handle for the JIT machine. The watchdog flips
    /// this on hard-abandon; the JIT observes at its next GC safepoint
    /// and returns `YieldError::Cancelled`. Separate from
    /// `SessionContext::cancel_state`: soft-cancel is a handler-level
    /// early return, hard-cancel is a JIT-level forced unwind. Keeping
    /// them distinct avoids escalating every soft cancel into a
    /// full JIT abort.
    jit_cancel: CancelHandle,
    /// In-memory active turn history + cached archive-summary head.
    /// Populated on session open via `TurnHistory::load` (when a DB is
    /// available) or `TurnHistory::empty` (tests). `run_turn` records
    /// each completed turn here; Task 13's compaction strategies
    /// consume the oldest entries.
    turn_history: Arc<std::sync::Mutex<TurnHistory>>,
}

/// Mutable per-session state guarded by [`TidepoolSession::inner`].
struct InnerState {
    machine: Option<Box<SessionMachine>>,
    bundle: Option<Box<SdkBundle>>,
    /// Set when a hard-abandon fires; further `step` calls short-circuit.
    poisoned: bool,
    /// Monotonic per-step turn counter for CheckpointEvent sequencing.
    turn_counter: u64,
}

impl std::fmt::Debug for TidepoolSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TidepoolSession")
            .field("session_id", &self.session_id)
            .field("agent_id", &self.ctx.agent_id())
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

    /// Test-only: flip the session's `poisoned` flag so the next `step`
    /// short-circuits with `RuntimeError::SessionPoisoned`. Used by the
    /// Phase 3 Task 18 / AC2.8 integration test to assert the poison
    /// short-circuit on its own without having to reproduce the exact
    /// race conditions that cause the real `run_turn` path to flip it
    /// (the JoinError branch is inherently non-deterministic under
    /// test).
    ///
    /// Feature-gated behind `test-hooks` so this never leaks into
    /// downstream consumers' builds. Pattern's own integration tests
    /// enable the feature via the self-dev-dep in `Cargo.toml`.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn __poison_for_tests(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.poisoned = true;
        }
    }

    /// Open a session for `persona`. Compiles the program, warms the JIT,
    /// constructs the handler bundle wired to `memory_store`, and records
    /// a fresh session id.
    ///
    /// Runs preflight first so missing tidepool-extract produces an
    /// actionable error before any work happens.
    pub fn open(
        persona: PersonaConfig,
        sdk: &SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
    ) -> Result<Self, RuntimeError> {
        crate::preflight::check()?;
        let sdk_dir = sdk.resolve()?;
        let program = compile_program(&persona.program, "agent", &sdk_dir)?;
        // 64 MiB matches tidepool-runtime's `DEFAULT_NURSERY_SIZE`. Smaller
        // nurseries trigger more GC cycles; upstream tidepool has an open bug
        // where long-running multi-module recursive agents can corrupt closure
        // pointers during GC in the JIT, manifesting as `[JIT] App: tag 255`.
        // 64 MiB sidesteps the corruption for the loop sizes used in tests;
        // reproduction lives at `crates/pattern_runtime/tests/recurse_repro.rs`.
        let nursery = persona.nursery_size.unwrap_or(64 * 1024 * 1024);
        let machine = SessionMachine::new(program, nursery)?;
        let jit_cancel = machine.cancel_handle();
        let session_id = pattern_core::types::ids::new_id().to_string();
        // Share the checkpoint log + current-turn counter between the
        // session and the handler-facing SessionContext so handlers'
        // `record_exchange` calls land in the same log the session
        // publishes via `TidepoolSession::checkpoint_log`.
        let checkpoint_log = Arc::new(std::sync::Mutex::new(CheckpointLog::new()));
        let current_turn = Arc::new(AtomicU64::new(0));
        let ctx = Arc::new(
            SessionContext::from_persona(&persona, memory_store, provider.clone())
                .with_checkpoint_log(checkpoint_log.clone(), current_turn.clone()),
        );

        let display = DisplayHandler::new();
        // Bundle order: Prelude-5 first, then rarer effects. See
        // `crates/pattern_runtime/src/sdk/bundle.rs` for the ordering
        // rationale (arity-aware DataCon disambiguation + backwards compat).
        let bundle: SdkBundle = frunk::hlist![
            MemoryHandler::new(ctx.memory_store()),
            MessageHandler,
            display.clone(),
            TimeHandler,
            LogHandler::for_session(session_id.clone()),
            ShellHandler,
            FileHandler,
            SourcesHandler,
            McpHandler,
            RpcHandler,
            SpawnHandler,
        ];

        Ok(Self {
            inner: std::sync::Mutex::new(InnerState {
                machine: Some(Box::new(machine)),
                bundle: Some(Box::new(bundle)),
                poisoned: false,
                turn_counter: 0,
            }),
            ctx,
            session_id,
            checkpoint_log,
            current_turn,
            display_handle: display,
            jit_cancel,
            turn_history: Arc::new(std::sync::Mutex::new(TurnHistory::empty())),
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

    /// Test-friendly step core: runs the machine, races the watchdog,
    /// returns a [`TurnOutput`] skeleton on success. Phase 3 does not
    /// surface rich turn output — `messages`, `block_writes` are empty.
    /// Phase 4+ wire these through the real MessageHandler / block-write
    /// collector.
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutput, RuntimeError> {
        // Scope the guard so we drop it before awaiting the spawn-blocking
        // task (holding a std::sync::MutexGuard across `.await` would
        // block the executor thread).
        let (mut machine, mut bundle) = {
            let mut inner = self.inner.lock().map_err(|_| RuntimeError::JoinError {
                reason: "inner mutex poisoned".into(),
            })?;
            if inner.poisoned {
                return Err(RuntimeError::SessionPoisoned {
                    reason:
                        "previous turn hard-abandoned due to runaway compute without effect yields"
                            .into(),
                });
            }
            inner.turn_counter += 1;
            // Publish the new turn number to the shared atomic so
            // handlers recording exchanges via SessionContext can stamp
            // them with the current turn.
            self.current_turn
                .store(inner.turn_counter, Ordering::SeqCst);
            let machine = inner
                .machine
                .take()
                .expect("machine present between turns (Option invariant)");
            let bundle = inner
                .bundle
                .take()
                .expect("bundle present between turns (Option invariant)");
            (machine, bundle)
        };
        let budget = self.ctx.budget();
        self.ctx.cancel_state.reset();
        // Clear any lingering cancel request from a previous turn. The
        // handle is Arc-shared with the JIT machine; resetting on both
        // ends keeps soft and hard paths independent.
        self.jit_cancel.reset();
        let ctx_clone = self.ctx.clone();

        let jit_handle = tokio::task::spawn_blocking(move || {
            let result = machine.run(&mut *bundle, ctx_clone.as_ref());
            // Return the moved-in state alongside the result so the
            // session can reinstate its fields on normal completion /
            // soft-cancel.
            (result, machine, bundle)
        });

        let watchdog_state = self.ctx.cancel_state.clone();
        let mut watchdog = crate::timeout::spawn_watchdog(
            watchdog_state.clone(),
            budget,
            std::time::Duration::from_millis(25),
        );

        // Race JIT vs watchdog. On hard-abandon the watchdog returns; we
        // detach the JIT task (which keeps running in the background
        // since we have no way to stop it) and poison the session.
        let mut jit_handle = jit_handle;
        tokio::select! {
            biased;
            join_result = &mut jit_handle => {
                watchdog.abort();
                let (run_result, machine, bundle) = join_result
                    .map_err(|e| RuntimeError::JoinError { reason: e.to_string() })?;
                // Reinstate state — even on error, so a soft-cancel
                // caller can step again.
                if let Ok(mut inner) = self.inner.lock() {
                    inner.machine = Some(machine);
                    inner.bundle = Some(bundle);
                }

                let cancelled = self.ctx.cancel_state.is_cancelled();
                self.ctx.cancel_state.reset();
                match run_result {
                    Ok(_value) => {
                        // Drain pending BlockWrites from the adapter into the
                        // TurnOutput. Phase 5: these feed pseudo-message emission.
                        let block_writes = self.ctx.adapter.drain_pending();
                        let output = TurnOutput {
                            messages: vec![],
                            block_writes,
                            usage: None,
                            cache_metrics: Default::default(),
                            completed_at: Timestamp::now(),
                        };

                        // Record in TurnHistory for the composer and
                        // compaction strategies.
                        if let Ok(mut hist) = self.turn_history.lock() {
                            hist.record(input.turn_id.clone(), output.clone());
                        }

                        Ok(output)
                    }
                    Err(e) if cancelled && is_cancel_sentinel(&e) => {
                        Err(RuntimeError::Timeout {
                            wall_ms: budget.wall.as_millis() as u64,
                            cpu_ms: budget.cpu.as_millis() as u64,
                            path: CancelPath::Soft,
                        })
                    }
                    Err(e) => Err(e),
                }
            }
            outcome = &mut watchdog => {
                match outcome {
                    Ok(crate::timeout::BoundedOutcome::HardAbandoned { wall_ms, cpu_ms }) => {
                        // The watchdog escalated because cooperative
                        // cancellation couldn't be delivered: no effect
                        // entries observed after the soft-cancel flag
                        // flipped. We flip the tidepool cancel flag; the
                        // JIT observes it at the next heap check (every
                        // non-trivial allocation) and unwinds cleanly
                        // with `YieldError::Cancelled`, which
                        // `error_map::map_yield_error` promotes to
                        // `RuntimeError::Timeout { path: HardAbandon }`
                        // with placeholder zeros. We then await the
                        // blocking task to reclaim the thread before
                        // returning. Typical observation latency on
                        // tight compute loops: ~20ms.
                        tracing::warn!(
                            session_id = %self.session_id,
                            wall_ms,
                            cpu_ms,
                            "hard-abandon: signalling tidepool CancelHandle; awaiting JIT unwind",
                        );
                        self.jit_cancel.cancel();

                        // Bound the await: a buggy or upstream-broken JIT
                        // that never reaches a heap-check safepoint would
                        // otherwise hang the entire runtime forever here.
                        // If we exceed `cancel_grace`, abort the blocking
                        // task (which detaches its thread — tokio has no
                        // way to actually stop blocking work), poison the
                        // session so no further steps reuse the machine,
                        // and return a RuntimeCrashed to tell the caller
                        // this is not a recoverable timeout.
                        let cancel_grace = budget.cancel_grace;
                        let join_result = match tokio::time::timeout(
                            cancel_grace,
                            &mut jit_handle,
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(_) => {
                                jit_handle.abort();
                                if let Ok(mut inner) = self.inner.lock() {
                                    inner.poisoned = true;
                                }
                                tracing::error!(
                                    session_id = %self.session_id,
                                    elapsed_ms = cancel_grace.as_millis() as u64,
                                    thread_id = ?std::thread::current().id(),
                                    "JIT failed to observe cancel within grace window; session poisoned, thread detached",
                                );
                                return Err(RuntimeError::RuntimeCrashed);
                            }
                        };
                        // Reset the cancel flag so a future turn (if the
                        // session stays clean) is not immediately
                        // cancelled on entry.
                        self.jit_cancel.reset();
                        self.ctx.cancel_state.reset();

                        let (run_result, machine, bundle) = match join_result {
                            Ok(triple) => triple,
                            Err(join_err) => {
                                // Blocking task panicked or was cancelled by
                                // the runtime — we cannot recover machine
                                // state. Poison and surface the join error;
                                // this is distinct from a clean cancel.
                                if let Ok(mut inner) = self.inner.lock() {
                                    inner.poisoned = true;
                                }
                                return Err(RuntimeError::JoinError {
                                    reason: join_err.to_string(),
                                });
                            }
                        };
                        // Reinstate state. Whether the JIT returned
                        // cleanly or with an unexpected error, the
                        // machine struct itself is structurally intact
                        // — unwinding happens through the normal
                        // Result return, not a panic.
                        if let Ok(mut inner) = self.inner.lock() {
                            inner.machine = Some(machine);
                            inner.bundle = Some(bundle);
                        }

                        match run_result {
                            // Expected path: the JIT observed the cancel
                            // flag at a heap check and returned our
                            // placeholder `Timeout { HardAbandon }`.
                            // Session remains clean — the next turn
                            // will reuse the machine.
                            Err(RuntimeError::Timeout {
                                path: CancelPath::HardAbandon,
                                ..
                            }) => Err(RuntimeError::Timeout {
                                wall_ms,
                                cpu_ms,
                                path: CancelPath::HardAbandon,
                            }),
                            // JIT returned some other error during the
                            // cancel race (e.g., finished normally
                            // before observing cancel, or crashed). The
                            // cancel still succeeded in unblocking us;
                            // surface the watchdog's verdict.
                            //
                            // Belt-and-suspenders poison: if we can't
                            // confirm clean cancellation we err on the
                            // side of not reusing the machine state.
                            _ => {
                                if let Ok(mut inner) = self.inner.lock() {
                                    inner.poisoned = true;
                                }
                                Err(RuntimeError::Timeout {
                                    wall_ms,
                                    cpu_ms,
                                    path: CancelPath::HardAbandon,
                                })
                            }
                        }
                    }
                    Ok(_) => Err(RuntimeError::WatchdogFailure),
                    Err(_) => Err(RuntimeError::WatchdogFailure),
                }
            }
        }
    }
}

/// Examine a `RuntimeError` produced by the JIT run path to decide
/// whether it was really our cancellation sentinel bubbling back out.
///
/// The JIT machine maps effect-handler errors to
/// `RuntimeError::SdkHandlerFailed { reason, .. }` via `error_map` (as
/// of the phase-3 review follow-up that split SDK-handler failure out
/// of CompileInternal); when our handlers emit the sentinel string, it
/// lands verbatim inside `reason`. We match on the rendered `Display`
/// rather than the specific variant so a future rehoming of the
/// sentinel through a different error-mapping still works — the
/// sentinel is stable-by-design and the predicate stays on its
/// observable identity. See [`crate::timeout::CANCELLED_SENTINEL`].
fn is_cancel_sentinel(e: &RuntimeError) -> bool {
    let s = e.to_string();
    s.contains(crate::timeout::CANCELLED_SENTINEL)
}

#[async_trait]
impl Session for TidepoolSession {
    async fn step(&mut self, input: TurnInput) -> Result<TurnOutput, RuntimeError> {
        // Internally run_turn uses `&self` (interior mutability via the
        // inner mutex); Session::step is `&mut self` per the core trait.
        self.run_turn(input).await
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
