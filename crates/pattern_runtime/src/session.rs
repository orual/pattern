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
use pattern_core::types::memory_types::MemoryError;
use pattern_core::types::snapshot::{PersonaSnapshot, SessionSnapshot};
use pattern_core::types::turn::{StepReply, TurnInput};

use crate::agent_loop::EvalWorker;
use crate::spawn::SpawnRegistry;

/// Compose the session's effective [`pattern_core::PolicySet`] from
/// runtime defaults plus the persona's KDL-loaded rules.
///
/// Order is irrelevant for evaluation correctness — `PolicySet::evaluate`
/// sorts by `Precedence` at lookup time — but constructing the vec
/// once at session open keeps allocation off the hot path.
///
/// Phase 1 Task 14 wires persona-level rules; project-level
/// `.pattern.kdl` policy and runtime overrides will layer in via
/// follow-up phases without changing this composition site (just
/// extend the iterator chain).
fn merge_policies(persona: &PersonaSnapshot) -> pattern_core::PolicySet {
    let defaults = crate::policy::rust_defaults();
    let kdl = persona.policy_rules.iter().cloned();
    pattern_core::PolicySet::from_rules(defaults.into_iter().chain(kdl))
}
use crate::checkpoint::{CheckpointEvent, CheckpointLog};
use crate::memory::{MemoryStoreAdapter, TurnHistory};
use crate::router::{RouterBridge, RouterRegistry};
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
    /// Optional slot-\[1\] override for the composer's system prompt.
    /// Threaded from `persona.system_prompt` at session open. When
    /// `Some`, the agent-loop composer substitutes this in place of
    /// [`pattern_core::DEFAULT_BASE_INSTRUCTIONS`]; `None` keeps the
    /// workspace default.
    system_prompt: Option<String>,
    /// Chat-options baseline threaded from `persona.model.chat_options`
    /// at session open. The agent loop clones this into the composer's
    /// `PartialRequest.options` and layers on streaming-capture flags
    /// (capture_usage, capture_content, capture_tool_calls,
    /// capture_reasoning_content). Persona-declared sampling, reasoning
    /// effort, verbosity, seed, stop_sequences, cache_control, etc. all
    /// reach the wire via this path.
    chat_options: genai::chat::ChatOptions,
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
    /// Constellation database handle. Required for message persistence
    /// and compaction (Pass B steps). Every session must have DB access;
    /// in-memory-only sessions are no longer supported.
    db: Arc<pattern_db::ConstellationDb>,
    /// Scheme-dispatched message router registry. Handlers dispatch
    /// Send/Reply/Notify through this. Set at session open; read-only
    /// thereafter.
    router: Arc<RouterRegistry>,
    /// Sync-to-async bridge for routing messages from the eval worker
    /// thread to the async router task. Lazily initialised by
    /// [`SessionContext::with_router`]; handlers call
    /// [`RouterBridge::route_sync`] instead of `Handle::current().block_on`.
    router_bridge: Option<RouterBridge>,
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
    /// Per-persona context policy: compression strategy, gate floors,
    /// and snapshot policy. Threaded from `persona.context` at session
    /// open. Consumed by the compaction driver (`crate::compaction`)
    /// before each wire turn.
    context_policy: pattern_core::types::snapshot::ContextPolicy,
    /// Session-scoped diagnostic events. Populated during session
    /// construction (e.g. lib-module compile failures) and read by the
    /// `Pattern.Diagnostics` effect handler. Read-only after construction.
    diagnostics: Arc<std::sync::Mutex<Vec<crate::sdk::handlers::diagnostics::DiagnosticEvent>>>,
    /// Capability set scoping which effects this session may invoke.
    /// `None` means "full power" — back-compat for sessions that pre-date
    /// capability scoping. Phase 2 spawn paths read this to restrict
    /// child sessions to a subset of the parent's capabilities.
    capabilities: Option<pattern_core::CapabilitySet>,
    /// Composed policy set: Rust defaults seeded at session open, with
    /// KDL config + runtime overrides layered on by Tasks 13/14. Read
    /// by handlers (Task 10 Shell, Task 15 File) before each effect
    /// dispatch.
    policies: Arc<pattern_core::PolicySet>,
    /// Per-runtime [`PermissionBroker`]. One broker per session — no
    /// global singleton. Phase 1's policy-evaluation handlers escalate
    /// to this broker via [`Self::permission_bridge`] when a
    /// `RequireApproval` rule fires.
    permission_broker: Arc<pattern_core::permission::PermissionBroker>,
    /// Sync-to-async bridge for handlers running on the eval-worker
    /// thread. `None` until [`Self::with_permission_bridge`] is called
    /// from an async context (typically `open_with_agent_loop`).
    permission_bridge: Option<Arc<crate::permission::PermissionBridge>>,
    /// Origin of the *immediate dispatcher* of an effect — i.e. who is
    /// asking right now, not what activated this turn. Written by
    /// `agent_loop::drive_step` per orchestrate iteration with
    /// `Author::Agent(self)` (the model is the immediate caller of every
    /// effect during normal model-driven flow); cleared on Drop
    /// (panic-safe via RAII guard).
    ///
    /// The activating turn's origin (which may be `Author::Partner(_)`)
    /// stays on the `TurnInput` for batch-type inference, persistence
    /// attribution, and routing. It is NOT what the broker's
    /// partner-bypass predicate reads — that distinction prevents the
    /// agent's autonomous activity from inheriting Partner authority on
    /// a Partner-activated turn.
    ///
    /// Future direct-execution paths (admin REPL, audited sandboxed
    /// code) may override this slot with a Partner origin before
    /// invoking a handler directly — that is the only path where the
    /// broker's partner-bypass actually fires. Phase 1 has none.
    current_dispatch_origin:
        Arc<std::sync::RwLock<Option<pattern_core::types::origin::MessageOrigin>>>,
    /// Registry tracking live child session handles spawned by this session.
    ///
    /// Enforces a per-parent concurrency limit on ephemeral children via a
    /// `tokio::sync::Semaphore`. When the parent session ends (this registry
    /// is dropped), all registered children have their cancel state flipped.
    ///
    /// The default limit of 8 is a conservative starting point for ensembles.
    /// Revisit when ensemble patterns in Phase 7 stress this ceiling.
    spawn_registry: Arc<SpawnRegistry>,
    /// GHC include paths threaded into the session's eval worker. Persisted
    /// here so child sessions (ephemerals, forks) can extend the parent's
    /// include set with their own synthesized lib directories without
    /// re-deriving the path list. Populated at session-open time by
    /// [`TidepoolSession::open_with_agent_loop`]; left as an empty `Arc<Vec>`
    /// for sessions constructed via `from_persona` directly (test paths
    /// that don't run an eval worker).
    include_paths: Arc<Vec<std::path::PathBuf>>,
    /// Caller-supplied tokio runtime handle. Borrowed for sync handler
    /// paths (e.g. the eval-worker thread) that need to `block_on` an
    /// async future without magic-capturing via `Handle::current()`.
    ///
    /// First consumer: the v3-multi-agent spawn handler. Ephemeral /
    /// AwaitSpawn / AwaitAll arms call `cx.user().tokio_handle().block_on`
    /// against the registry's `Shared<BoxFuture<SpawnResult>>`. The
    /// sandbox-io Phase 3 PortRegistry actor will reuse the same handle
    /// when it lands.
    ///
    /// Note on existing bridges: `PermissionBridge` could later migrate
    /// to this approach (broker calls are well-bounded; no plugin code
    /// in the await path). `RouterBridge` deliberately stays as a bridge
    /// — router endpoints may dispatch to plugin-provided code where the
    /// await path crosses arbitrary user code, and the sync→async glue
    /// keeps the eval worker isolated from that risk.
    tokio_handle: tokio::runtime::Handle,
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

/// Handlers call this to read the active [`pattern_core::PolicySet`].
///
/// `SessionContext` exposes the live, KDL-merged set; the `()` shim
/// returns an always-empty set so unit tests using `&()` see every
/// effect as [`pattern_core::PolicyAction::Allow`] (i.e. they fall
/// straight through to the handler's existing "no gate" path).
pub trait HasPolicySet {
    fn policies(&self) -> &pattern_core::PolicySet;
}

impl HasPolicySet for SessionContext {
    fn policies(&self) -> &pattern_core::PolicySet {
        SessionContext::policies(self)
    }
}

impl HasPolicySet for () {
    fn policies(&self) -> &pattern_core::PolicySet {
        static EMPTY: std::sync::OnceLock<pattern_core::PolicySet> = std::sync::OnceLock::new();
        EMPTY.get_or_init(pattern_core::PolicySet::new)
    }
}

/// Handlers call this to consult the per-session
/// [`crate::permission::PermissionBridge`] and the current turn's
/// originator.
///
/// `SessionContext` provides the live wiring; the no-op `()` impl lets
/// unit tests pass `&()` as the user value (handlers will observe the
/// gate as missing and fall back to allow-by-default policy paths or
/// surface a clear error).
pub trait HasPermissionBridge {
    /// Sync-to-async bridge to the per-session broker. `None` for
    /// sessions that haven't been wired yet (or for the `()` test
    /// shim).
    fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>>;

    /// Origin of the immediate dispatcher of the current effect —
    /// `Author::Agent(self)` during normal model-driven dispatch; can
    /// be a `Partner` only when a future direct-execution path
    /// explicitly overrides the slot before invoking a handler.
    fn current_dispatch_origin(&self) -> Option<pattern_core::types::origin::MessageOrigin>;

    /// Agent identifier for broker attribution. The broker's
    /// `scope_cache` is keyed `(agent_id, scope)`; using the real
    /// session agent here is **load-bearing for per-agent isolation** —
    /// two agents in the same runtime asking for the same scope must
    /// NOT share a single grant. Returning `None` (the `()` shim's
    /// behaviour) tells handlers to fail closed: the broker call is
    /// skipped and the request is treated as a denial.
    fn dispatch_agent_id(&self) -> Option<pattern_core::AgentId>;
}

impl HasPermissionBridge for SessionContext {
    fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>> {
        SessionContext::permission_bridge(self)
    }

    fn current_dispatch_origin(&self) -> Option<pattern_core::types::origin::MessageOrigin> {
        SessionContext::current_dispatch_origin(self)
    }

    fn dispatch_agent_id(&self) -> Option<pattern_core::AgentId> {
        Some(pattern_core::AgentId::from(SessionContext::agent_id(self)))
    }
}

impl HasPermissionBridge for () {
    fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>> {
        None
    }
    fn current_dispatch_origin(&self) -> Option<pattern_core::types::origin::MessageOrigin> {
        None
    }
    fn dispatch_agent_id(&self) -> Option<pattern_core::AgentId> {
        None
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

/// Handlers call this to reach the per-session [`SpawnRegistry`].
///
/// `SessionContext` exposes the live registry; the `()` shim returns a
/// shared zero-limit registry so unit tests using `&()` as their user
/// context compile without error. The `()` registry's limit of 0 means
/// all `try_acquire_ephemeral_slot` calls return `None` — appropriate for
/// handler unit tests that are not testing spawn semantics.
pub trait HasSpawnRegistry {
    /// Per-session spawn registry. Handlers use this to acquire slots,
    /// register child handles, and surface the concurrency limit in errors.
    fn spawn_registry(&self) -> &Arc<SpawnRegistry>;
}

impl HasSpawnRegistry for SessionContext {
    fn spawn_registry(&self) -> &Arc<SpawnRegistry> {
        &self.spawn_registry
    }
}

impl HasSpawnRegistry for () {
    fn spawn_registry(&self) -> &Arc<SpawnRegistry> {
        // Zero-limit registry shared across all `()` calls. Unit tests
        // that use `&()` as their user context are not testing spawn
        // semantics; a limit-0 registry ensures no accidental spawns while
        // satisfying the trait bound.
        static SHIM: std::sync::OnceLock<Arc<SpawnRegistry>> = std::sync::OnceLock::new();
        SHIM.get_or_init(|| Arc::new(SpawnRegistry::new("test-shim", 0)))
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
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        let agent_id = persona.agent_id.to_string();
        let budget = Budget::from_persona(persona);
        let adapter = Arc::new(MemoryStoreAdapter::new(memory_store, &agent_id));
        // Default concurrency limit of 8: a conservative starting point for
        // ensemble patterns. Revisit when Phase 7 ensemble patterns stress
        // this ceiling. The agent_id is the natural parent identifier at this
        // stage; TidepoolSession::open will have the session_id but from_persona
        // does not — agent_id is stable and unambiguous as a parent label.
        let spawn_registry = Arc::new(SpawnRegistry::new(agent_id.clone(), 8));
        Self {
            agent_id,
            // Thread the caller's declared model through so the composer's
            // `ctx.model_id()` matches the persona's intent. Callers that
            // want to override a persona's default at open time should
            // mutate `persona.model.choice` before calling into the
            // runtime.
            model_id: persona.model.choice.model_id.to_string(),
            system_prompt: persona.system_prompt.clone(),
            chat_options: persona.model.chat_options.clone(),
            budget,
            cancel_state: Arc::new(CancelState::new()),
            adapter,
            provider,
            db,
            router: Arc::new(RouterRegistry::new()),
            router_bridge: None,
            pending_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
            turn_sink: Arc::new(NoOpSink),
            checkpoint_log: Arc::new(std::sync::Mutex::new(CheckpointLog::new())),
            current_turn: Arc::new(AtomicU64::new(0)),
            snapshot_policy: persona.context.snapshot_policy.clone(),
            context_policy: persona.context.clone(),
            diagnostics: Arc::new(std::sync::Mutex::new(Vec::new())),
            capabilities: persona.capabilities.clone(),
            policies: Arc::new(merge_policies(persona)),
            permission_broker: Arc::new(pattern_core::permission::PermissionBroker::new()),
            permission_bridge: None,
            current_dispatch_origin: Arc::new(std::sync::RwLock::new(None)),
            spawn_registry,
            tokio_handle,
            include_paths: Arc::new(Vec::new()),
        }
    }

    /// Replace the session's include-paths set. Called by
    /// [`TidepoolSession::open_with_agent_loop`] after lib-module
    /// validation; child-session forks (`fork_for_ephemeral`) read this
    /// to inherit the parent's resolved set.
    pub(crate) fn set_include_paths(&mut self, paths: Arc<Vec<std::path::PathBuf>>) {
        self.include_paths = paths;
    }

    /// Replace the session's spawn registry with a fresh one carrying
    /// a custom concurrency ceiling. Only available under
    /// `feature = "test-support"` — production code MUST use the
    /// default ceiling threaded via `from_persona`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_spawn_registry_for_test(&mut self, limit: usize) {
        self.spawn_registry = Arc::new(SpawnRegistry::new(self.agent_id.clone(), limit));
    }

    /// GHC include paths threaded into this session's eval worker.
    ///
    /// Empty for sessions constructed via `from_persona` without an
    /// eval worker (test paths). Populated at session-open time by
    /// [`TidepoolSession::open_with_agent_loop`].
    pub fn include_paths(&self) -> &Arc<Vec<std::path::PathBuf>> {
        &self.include_paths
    }

    /// Build a child session context for an ephemeral spawn.
    ///
    /// What's shared (Arc-cloned from parent): provider, db, router,
    /// adapter (MemoryStoreAdapter — child reads parent's memory),
    /// cancel_state (parent's cancel propagates to child), tokio_handle.
    ///
    /// What's fresh: pending_messages, checkpoint_log, current_turn,
    /// spawn_registry (sub-registry), current_dispatch_origin, diagnostics,
    /// permission_broker (no bridge yet — handler-side concern).
    ///
    /// What's overridden: capabilities (caller-supplied subset),
    /// system_prompt (`cfg.costume` when set; otherwise inherits parent's),
    /// agent_id (same as parent — persona identity stays in logs per
    /// AC3.3).
    ///
    /// Returns the constructed child context as `Arc<SessionContext>`.
    /// Caller is responsible for the spawn-lib synthesis + child
    /// include-path extension.
    pub fn fork_for_ephemeral(
        &self,
        cfg: &pattern_core::spawn::EphemeralConfig,
        child_caps: pattern_core::CapabilitySet,
        child_include_paths: Arc<Vec<std::path::PathBuf>>,
    ) -> Arc<SessionContext> {
        // Sub-registry concurrency limit. Half the parent's default is a
        // conservative starting point — ensembles-of-ensembles are a
        // Phase 7 concern; revisit when the workload demands it.
        let sub_limit: usize = (self.spawn_registry.concurrent_ephemeral_limit() / 2).max(1);
        let child_registry = Arc::new(SpawnRegistry::new(self.agent_id.clone(), sub_limit));

        // Costume override: replace the system_prompt slot when set;
        // otherwise inherit parent's.
        let system_prompt = cfg.costume.clone().or_else(|| self.system_prompt.clone());

        let child = SessionContext {
            agent_id: self.agent_id.clone(),
            model_id: self.model_id.clone(),
            system_prompt,
            chat_options: self.chat_options.clone(),
            budget: self.budget,
            // Shared cancel state — parent cancel propagates to child.
            cancel_state: self.cancel_state.clone(),
            // Shared adapter — child reads parent's memory. Write
            // restriction is enforced by the child's capability set
            // (the caller restricted it via restrict_to() before this
            // call).
            adapter: self.adapter.clone(),
            provider: self.provider.clone(),
            db: self.db.clone(),
            router: self.router.clone(),
            // No router bridge: the child runs without RouterBridge
            // wired; messaging effects must be opt-in via the child's
            // CapabilitySet.
            router_bridge: None,
            pending_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
            // Inherit parent's turn sink so the child's display events
            // surface in the same place as the parent's. Subscribers
            // should disambiguate by agent_id when needed.
            turn_sink: self.turn_sink.clone(),
            checkpoint_log: Arc::new(std::sync::Mutex::new(CheckpointLog::new())),
            current_turn: Arc::new(AtomicU64::new(0)),
            snapshot_policy: self.snapshot_policy.clone(),
            context_policy: self.context_policy.clone(),
            diagnostics: Arc::new(std::sync::Mutex::new(Vec::new())),
            capabilities: Some(child_caps),
            policies: self.policies.clone(),
            permission_broker: Arc::new(pattern_core::permission::PermissionBroker::new()),
            // Permission bridge is None; ephemerals don't currently
            // route gated effects through the broker (Phase 4+ may
            // revisit).
            permission_bridge: None,
            current_dispatch_origin: Arc::new(std::sync::RwLock::new(None)),
            spawn_registry: child_registry,
            tokio_handle: self.tokio_handle.clone(),
            include_paths: child_include_paths,
        };
        Arc::new(child)
    }

    /// Caller-supplied tokio runtime handle. Borrowed for sync handler
    /// paths that need to `block_on` an async future without
    /// magic-capturing via `Handle::current()`.
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle {
        &self.tokio_handle
    }

    /// Active policy set for this session. Handlers consult this
    /// before each effect dispatch; the result drives the broker
    /// escalation decision.
    pub fn policies(&self) -> &Arc<pattern_core::PolicySet> {
        &self.policies
    }

    /// Builder-style: replace the policy set (Phase 1 Task 14 wires
    /// KDL + runtime overrides over the seeded defaults).
    #[must_use]
    pub fn with_policies(mut self, policies: Arc<pattern_core::PolicySet>) -> Self {
        self.policies = policies;
        self
    }

    /// Per-runtime [`pattern_core::permission::PermissionBroker`]. Each
    /// session owns its own broker — there is no shared singleton.
    pub fn permission_broker(&self) -> &Arc<pattern_core::permission::PermissionBroker> {
        &self.permission_broker
    }

    /// Sync-to-async bridge to the broker, used by handlers running on
    /// the eval-worker thread. `None` until
    /// [`Self::with_permission_bridge`] has been called.
    pub fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>> {
        self.permission_bridge.as_ref()
    }

    /// Origin of the immediate dispatcher invoking an effect during
    /// the active orchestrate iteration. Returns `Author::Agent(self)`
    /// during normal model-driven dispatch — handlers consult this
    /// (not the activating turn's origin) when feeding the broker's
    /// partner-bypass predicate, so autonomous agent activity does
    /// not inherit Partner authority on Partner-activated turns.
    pub fn current_dispatch_origin(&self) -> Option<pattern_core::types::origin::MessageOrigin> {
        self.current_dispatch_origin.read().ok()?.clone()
    }

    /// Internal handle to the current-dispatch-origin slot. Used by
    /// `agent_loop::drive_step`'s RAII guard to write the origin per
    /// orchestrate iteration and clear it on Drop.
    pub(crate) fn current_dispatch_origin_slot(
        &self,
    ) -> &Arc<std::sync::RwLock<Option<pattern_core::types::origin::MessageOrigin>>> {
        &self.current_dispatch_origin
    }

    /// Builder-style: install a [`crate::permission::PermissionBridge`]
    /// pumping this session's broker. Must be called from an async
    /// context (the bridge spawns a tokio task).
    #[must_use]
    pub fn with_permission_bridge(
        mut self,
        bridge: Arc<crate::permission::PermissionBridge>,
    ) -> Self {
        self.permission_bridge = Some(bridge);
        self
    }

    /// Effective capabilities for this session.
    ///
    /// `None` means "full power" — sessions that pre-date capability
    /// scoping or that omit a capability set on open. Phase 2 spawn
    /// paths use this to enforce that children cannot escalate
    /// beyond the parent.
    pub fn capabilities(&self) -> Option<&pattern_core::CapabilitySet> {
        self.capabilities.as_ref()
    }

    /// Builder-style: set this session's capability set. Pass `None`
    /// to leave capabilities unscoped.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Option<pattern_core::CapabilitySet>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Persona-supplied slot-\[1\] system prompt override, if any.
    /// Composer consumes this in `compose_request_for_turn` when
    /// building the system-blocks array; `None` falls through to
    /// [`pattern_core::DEFAULT_BASE_INSTRUCTIONS`].
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    /// Baseline [`genai::chat::ChatOptions`] for requests composed in
    /// this session. Callers clone and layer on per-turn overrides
    /// (streaming capture flags, etc.). Persona-declared temperature,
    /// reasoning_effort, max_tokens, stop_sequences, etc. originate
    /// here.
    pub fn chat_options(&self) -> &genai::chat::ChatOptions {
        &self.chat_options
    }

    /// Wrap the underlying memory store in a [`pattern_memory::scope::MemoryScope`]
    /// with the given binding. This inserts the scope layer between the
    /// adapter and the raw store, enabling persona isolation per the
    /// [`IsolatePolicy`](pattern_core::types::memory_types::IsolatePolicy).
    ///
    /// Must be called before the session is shared (i.e., before
    /// `Arc::new(ctx)` in `TidepoolSession::open`). Calling after the
    /// adapter has been cloned elsewhere is a logic error (but harmless —
    /// only the original adapter sees the scope).
    #[must_use]
    pub fn with_scope_binding(mut self, binding: pattern_memory::scope::ScopeBinding) -> Self {
        use pattern_memory::scope::MemoryScope;
        let old_inner = self.adapter.inner().clone();
        let scoped: Arc<dyn MemoryStore> = Arc::new(MemoryScope::new(old_inner, binding));
        self.adapter = Arc::new(MemoryStoreAdapter::new(scoped, &self.agent_id));
        self
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

    /// Constellation database handle. Used for message persistence,
    /// turn-history loading, and compaction.
    pub fn db(&self) -> &Arc<pattern_db::ConstellationDb> {
        &self.db
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

    /// Per-persona context policy (compression, gate floors, snapshot).
    /// Consumed by `crate::compaction::maybe_compact` before each wire
    /// turn in `drive_step`.
    pub fn context_policy(&self) -> &pattern_core::types::snapshot::ContextPolicy {
        &self.context_policy
    }

    /// Session diagnostics. Accumulated during construction; read by
    /// the `Pattern.Diagnostics` handler.
    pub fn diagnostics(
        &self,
    ) -> &Arc<std::sync::Mutex<Vec<crate::sdk::handlers::diagnostics::DiagnosticEvent>>> {
        &self.diagnostics
    }

    /// Scheme-dispatched router registry for message routing.
    pub fn router(&self) -> &Arc<RouterRegistry> {
        &self.router
    }

    /// Sync-to-async router bridge. Returns `None` if no router has
    /// been wired via [`Self::with_router`]. Handlers should prefer
    /// this over direct `router()` access — it is safe to call from
    /// a plain OS thread without a tokio runtime context.
    pub fn router_bridge(&self) -> Option<&RouterBridge> {
        self.router_bridge.as_ref()
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

    /// Per-session spawn registry. Tracks live child handles, enforces
    /// the ephemeral concurrency limit, and cancels all children when
    /// the registry is dropped.
    pub fn spawn_registry(&self) -> &Arc<SpawnRegistry> {
        &self.spawn_registry
    }

    /// Replace the router registry and spawn the async router bridge.
    /// Used by session open (and tests) to inject a pre-configured
    /// registry — typically registered with a `CliRouter` or other
    /// scheme handlers before the session starts.
    ///
    /// Must be called from within a tokio runtime context (the bridge
    /// spawns a tokio task). After this call, handlers can use
    /// [`Self::router_bridge`] to dispatch messages from a plain OS
    /// thread without needing `Handle::current()`.
    #[allow(dead_code)]
    pub(crate) fn with_router(mut self, router: Arc<RouterRegistry>) -> Self {
        self.router_bridge = Some(RouterBridge::spawn(router.clone()));
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

    /// Agent id this session runs as (delegated from [`SessionContext`]).
    pub fn agent_id(&self) -> &str {
        self.ctx.agent_id()
    }

    /// Accessor for the checkpoint log — exposed so tests can assert on
    /// recorded events.
    pub fn checkpoint_log(&self) -> Arc<std::sync::Mutex<CheckpointLog>> {
        self.checkpoint_log.clone()
    }

    /// The session's shared cancel state. Callers can call
    /// [`CancelState::request_cancel`] to soft-cancel a running step.
    pub fn cancel_state(&self) -> Arc<CancelState> {
        self.ctx.cancel_state()
    }

    /// The Haskell preamble built for this session at open-time.
    /// Returns `None` for sessions opened via [`Self::open`] (no eval
    /// worker, no preamble); `Some(_)` for sessions opened via
    /// [`Self::open_with_agent_loop`].
    ///
    /// Capability-scoped open paths (Phase 1) build the preamble via
    /// [`crate::sdk::preamble::build_for`], so the returned string
    /// already has effects absent from the session's capability set
    /// stripped from imports and the `type M` row.
    pub fn preamble(&self) -> Option<&str> {
        self.preamble.as_deref()
    }

    /// Shared handle to the session's [`SessionContext`]. Tests can
    /// borrow this as the `user` argument to
    /// `tidepool_runtime::compile_and_run` when exercising the
    /// agent-loop substrate without driving a full step.
    pub fn context(&self) -> Arc<SessionContext> {
        self.ctx.clone()
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
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
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
            SessionContext::from_persona(
                &persona,
                memory_store,
                provider.clone(),
                db,
                tokio_handle,
            )
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
    #[allow(clippy::too_many_arguments)]
    pub async fn open_with_agent_loop(
        persona: PersonaSnapshot,
        sdk: &SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
        turn_sink: Arc<dyn TurnSink>,
        prelude_dir: Option<PathBuf>,
        mount_path: Option<PathBuf>,
        capabilities: Option<pattern_core::CapabilitySet>,
    ) -> Result<Self, RuntimeError> {
        // Capture persona-scoped state we'll seed into the store after the
        // session is constructed. We consume `persona` via `Self::open`
        // below; extracting these now keeps the rest of the open path
        // simple.
        let agent_id_for_seed = persona.agent_id.to_string();
        let memory_blocks_for_seed = persona.memory_blocks.clone();
        let store_for_seed = memory_store.clone();

        // Initialise the base session (preflight, context, checkpoint log).
        let mut session = Self::open(persona, sdk, memory_store, provider, db, tokio_handle)?;

        // Seed persona-declared memory blocks into the store. Blocks that
        // already exist (e.g. restored from a persistent DB on re-spawn)
        // are left as-is — persona declares INITIAL content; live state
        // wins.
        seed_persona_memory_blocks(
            &*store_for_seed,
            &agent_id_for_seed,
            &memory_blocks_for_seed,
        )?;

        // Replace the NoOpSink on the freshly constructed SessionContext.
        // We have exclusive ownership of `session` here (just returned
        // from open), so Arc::try_unwrap on ctx will always succeed.
        let ctx_owned =
            Arc::try_unwrap(session.ctx).expect("ctx has no other clones immediately after open()");
        // Spawn a permission bridge over this session's broker. Must
        // happen in async context (bridge spawns a tokio task).
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(
            ctx_owned.permission_broker().clone(),
        ));
        let ctx_with_sink = ctx_owned
            .with_turn_sink(turn_sink.clone())
            .with_capabilities(capabilities.clone())
            .with_permission_bridge(bridge);

        // Wire MemoryScope if a mount config declares an isolation policy.
        // Must happen before Arc::new(ctx) so the scope wraps the store
        // before any other reference to ctx exists.
        let ctx_with_scope = if let Some(mount) = mount_path.as_deref() {
            let kdl_path = mount.join(".pattern.kdl");
            match pattern_memory::config::load_mount_config(&kdl_path) {
                Ok(mount_config) => {
                    // Resolve the policy; default to None on validation error
                    // (log the issue but don't abort session open).
                    let policy = match mount_config.isolate_from_persona.resolve() {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "failed to resolve isolate_from_persona policy; \
                                 defaulting to IsolatePolicy::None"
                            );
                            pattern_core::types::memory_types::IsolatePolicy::None
                        }
                    };
                    let binding = pattern_memory::scope::ScopeBinding::with_project(
                        agent_id_for_seed.clone(),
                        mount_config.project.name.clone(),
                        policy,
                    );
                    ctx_with_sink.with_scope_binding(binding)
                }
                Err(e) => {
                    // Mount config missing or malformed. Log a warning and
                    // proceed without a scope: the session is still valid,
                    // just without project isolation.
                    tracing::warn!(
                        path = %kdl_path.display(),
                        error = %e,
                        "could not load .pattern.kdl for scope wiring; \
                         proceeding without MemoryScope"
                    );
                    ctx_with_sink
                }
            }
        } else {
            ctx_with_sink
        };

        // Build the shared preamble once per session, scoped to the
        // caller-supplied capability set. `None` keeps the full canonical
        // row — back-compat for sessions that pre-date capability scoping.
        let preamble = match capabilities.as_ref() {
            Some(caps) => crate::sdk::preamble::build_for(caps),
            None => crate::sdk::preamble::build(&crate::sdk::bundle::canonical_effect_decls()),
        };

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

        // Extend include path with `<mount>/lib/` if present.
        // Approach A: probe-compile each module individually via
        // compile_haskell. See `sdk::lib_modules` for details.
        let lib_failures = if let Some(mount) = mount_path.as_deref() {
            let lib_validation =
                crate::sdk::lib_modules::validate_and_resolve(mount, &include_paths);
            include_paths.extend(lib_validation.successful_paths);
            lib_validation.failures
        } else {
            Vec::new()
        };

        // Persist the resolved include path on SessionContext BEFORE the
        // final Arc-wrap so child-session forks (Phase 2 spawn) can
        // inherit them. Stash diagnostics from any lib-compile failures
        // here too, while we still have `&mut` access on the inner ctx.
        let mut ctx_with_paths = ctx_with_scope;
        ctx_with_paths.set_include_paths(Arc::new(include_paths.clone()));
        if !lib_failures.is_empty() {
            let mut diags = ctx_with_paths
                .diagnostics
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            diags.extend(
                lib_failures
                    .into_iter()
                    .map(crate::sdk::handlers::diagnostics::DiagnosticEvent::from),
            );
        }

        session.ctx = Arc::new(ctx_with_paths);

        // Wire the turn sink into the DisplayHandler so Display events
        // flow to CLI/TUI subscribers during eval turns.
        session.display_handle.forward_to_turn_sink(turn_sink);

        // Spawn the eval worker.
        let worker = EvalWorker::spawn_with_includes(
            session.ctx.clone(),
            include_paths,
            session.session_id.clone(),
        );

        session.eval_worker = Some(worker);
        session.preamble = Some(preamble);

        // Restore turn history from persisted messages so re-spawning
        // against the same data-dir resumes conversation state.
        if let Err(e) = session.load_turn_history(session.ctx.db()).await {
            tracing::warn!(
                error = %e,
                "failed to restore turn history from DB; starting with empty history"
            );
        }

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

/// Seed persona-declared memory blocks into the store at session open.
///
/// For each `MemoryBlockSpec` in `persona.memory_blocks`:
/// - If a block with the same label already exists (e.g. restored from a
///   persistent DB on re-spawn), leave it untouched. Persona declares
///   INITIAL content; live state wins.
/// - Otherwise create the block via the trait's `create_block`, feed
///   `spec.content` through `StructuredDocument::import_from_json`
///   (schema-dispatched), apply `pinned` via `set_block_pinned`, then
///   persist.
///
/// `crdt_snapshot` is currently always `None` in foundation; when the
/// full-CRDT restore path lands, this helper will need to branch on it.
fn seed_persona_memory_blocks(
    store: &dyn MemoryStore,
    agent_id: &str,
    memory_blocks: &std::collections::HashMap<
        smol_str::SmolStr,
        pattern_core::types::snapshot::MemoryBlockSpec,
    >,
) -> Result<(), RuntimeError> {
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, MemoryType};

    for (label, spec) in memory_blocks {
        // shared_id is a planned feature for constellation-level cross-agent
        // block sharing. The resolver is not wired yet, so fail loudly rather
        // than silently ignoring the field and leaving the agent with wrong
        // memory configuration.
        if let Some(shared_id) = &spec.shared_id {
            return Err(RuntimeError::SharedBlockRefNotSupported {
                label: label.to_string(),
                shared_id: shared_id.to_string(),
            });
        }

        // Don't clobber existing blocks — persona is INITIAL intent.
        // The store may return Err(NotFound) or Ok(None) for missing blocks
        // depending on the implementation. Both mean "create it".
        match store.get_block(agent_id, label.as_str()) {
            Ok(Some(_)) => continue, // Already exists — preserve live state.
            Ok(None) => {}           // Doesn't exist — create below.
            Err(MemoryError::NotFound { .. }) => {} // Store returns Err for missing — treat as "create."
            Err(e) => {
                return Err(RuntimeError::MemorySeedFailed {
                    label: label.to_string(),
                    reason: format!("get_block failed: {e}"),
                });
            }
        }

        let block_type = match spec.memory_type {
            MemoryType::Core => MemoryBlockType::Core,
            // Archival persona specs create Working-tier blocks; true
            // archival storage lives in archival_entries (separate table).
            MemoryType::Working | MemoryType::Archival => MemoryBlockType::Working,
        };
        let schema = spec.schema.clone().unwrap_or_else(BlockSchema::text);

        let mut create = BlockCreate::new(label.as_str(), block_type, schema)
            // Thread the persona-declared permission through to the store.
            // Without this, BlockCreate defaults to ReadWrite, silently
            // upgrading any persona-declared ReadOnly block.
            .with_permission(spec.permission);
        if let Some(desc) = &spec.description {
            create = create.with_description(desc.clone());
        }
        if let Some(limit) = spec.char_limit {
            create = create.with_char_limit(limit);
        }

        let doc =
            store
                .create_block(agent_id, create)
                .map_err(|e| RuntimeError::MemorySeedFailed {
                    label: label.to_string(),
                    reason: format!("create_block failed: {e}"),
                })?;

        // Schema-dispatched import of the initial content.
        doc.import_from_json(&spec.content)
            .map_err(|e| RuntimeError::MemorySeedFailed {
                label: label.to_string(),
                reason: format!("import_from_json failed: {e:?}"),
            })?;

        if spec.pinned {
            store
                .update_block_metadata(
                    agent_id,
                    label.as_str(),
                    pattern_core::types::memory_types::BlockMetadataPatch::default().pinned(true),
                )
                .map_err(|e| RuntimeError::MemorySeedFailed {
                    label: label.to_string(),
                    reason: format!("update_block_metadata failed: {e}"),
                })?;
        }

        store.persist_block(agent_id, label.as_str()).map_err(|e| {
            RuntimeError::MemorySeedFailed {
                label: label.to_string(),
                reason: format!("persist_block failed: {e}"),
            }
        })?;
    }
    Ok(())
}

// ---- session tests -------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::SdkLocation;
    use crate::testing::{InMemoryMemoryStore, MockProviderClient};
    use pattern_core::ProviderClient;
    use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
    use pattern_core::types::ids::{BatchId, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use pattern_core::types::snapshot::PersonaSnapshot;
    use pattern_core::types::turn::StopReason;

    fn test_turn_input() -> TurnInput {
        // Fresh batch start: turn_id == batch_id (first turn IS the batch).
        let id = new_snowflake_id();
        TurnInput {
            turn_id: id.clone(),
            batch_id: BatchId::from(id),
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
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();

        let session = TidepoolSession::open(
            persona,
            &sdk,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        )
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
        let db = crate::testing::test_db().await;
        // Create the agent row so the FK on messages.agent_id is satisfied
        // when drive_step persists messages.
        {
            let agent = pattern_db::models::Agent {
                id: "agent-a".to_string(),
                name: "Test".to_string(),
                description: None,
                model_provider: "test".to_string(),
                model_name: "test-model".to_string(),
                system_prompt: "test".to_string(),
                config: pattern_db::Json(serde_json::json!({})),
                enabled_tools: pattern_db::Json(vec![]),
                tool_rules: None,
                status: pattern_db::models::AgentStatus::Active,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            pattern_db::queries::create_agent(&db.get().unwrap(), &agent)
                .expect("create test agent");
        }

        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();

        let session = TidepoolSession::open_with_agent_loop(
            persona,
            &sdk,
            store,
            provider_dyn,
            db,
            tokio::runtime::Handle::current(),
            sink_dyn,
            None,
            None,
            None,
        )
        .await
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
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();

        let session = TidepoolSession::open_with_agent_loop(
            persona,
            &sdk,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
            sink_dyn,
            None,
            None,
            None,
        )
        .await
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

    /// `seed_persona_memory_blocks` must thread the persona-declared
    /// `MemoryPermission` through to the underlying store. Without the fix,
    /// `BlockCreate` always defaulted to `ReadWrite`, silently upgrading any
    /// persona-declared `ReadOnly` block.
    ///
    /// Regression test for fix #2 (code-review finding: MemoryBlockSpec
    /// .permission not threaded through BlockCreate to MemoryCache).
    #[tokio::test]
    async fn seed_persona_memory_blocks_threads_permission_to_store() {
        use pattern_core::types::memory_types::MemoryPermission;
        use pattern_core::types::snapshot::MemoryBlockSpec;

        let store = Arc::new(InMemoryMemoryStore::new());
        let store_dyn: Arc<dyn MemoryStore> = store.clone();

        let persona = PersonaSnapshot::new("agent-perm", "Permission test agent")
            .with_memory_block(
                "persona",
                MemoryBlockSpec::text("I am a read-only persona block.")
                    .with_permission(MemoryPermission::ReadOnly),
            )
            .with_memory_block(
                "scratchpad",
                MemoryBlockSpec::text("mutable notes").with_permission(MemoryPermission::ReadWrite),
            );

        seed_persona_memory_blocks(store_dyn.as_ref(), "agent-perm", &persona.memory_blocks)
            .expect("seed should succeed");

        // Check the read-only block — permission must be preserved.
        let doc = store_dyn
            .get_block("agent-perm", "persona")
            .expect("get_block should succeed")
            .expect("persona block should exist");
        assert_eq!(
            doc.permission(),
            pattern_core::types::memory_types::MemoryPermission::ReadOnly,
            "persona block should be ReadOnly as declared in the spec"
        );

        // Check the read-write block — default must round-trip correctly.
        let doc2 = store_dyn
            .get_block("agent-perm", "scratchpad")
            .expect("get_block should succeed")
            .expect("scratchpad block should exist");
        assert_eq!(
            doc2.permission(),
            pattern_core::types::memory_types::MemoryPermission::ReadWrite,
            "scratchpad block should be ReadWrite as declared in the spec"
        );
    }

    /// `seed_persona_memory_blocks` must reject any block that declares
    /// `shared_id`. Shared block references are not supported in the
    /// foundation runtime; silently ignoring the field would leave the agent
    /// with wrong memory configuration.
    ///
    /// Regression test for fix #3 (code-review finding: MemoryBlockSpec
    /// .shared_id is not validated at seed time).
    #[tokio::test]
    async fn seed_persona_memory_blocks_rejects_shared_id() {
        use pattern_core::error::RuntimeError;
        use pattern_core::types::snapshot::MemoryBlockSpec;
        use smol_str::SmolStr;

        let store = Arc::new(InMemoryMemoryStore::new());
        let store_dyn: Arc<dyn MemoryStore> = store.clone();

        // Build a spec with a shared_id set. We need to go through the
        // `Default` + field mutation path because `MemoryBlockSpec` is
        // `#[non_exhaustive]` so struct expressions are not usable outside
        // `pattern_core`. Use the `with_shared_id` builder if it exists;
        // otherwise mutate directly via the public field (it is `pub`).
        let mut spec_with_shared = MemoryBlockSpec::text("this content should never be used");
        spec_with_shared.shared_id = Some(SmolStr::new("mem_01HXYZ_shared"));

        let persona = PersonaSnapshot::new("agent-shared", "Shared block test agent")
            .with_memory_block("shared_notes", spec_with_shared);

        let result =
            seed_persona_memory_blocks(store_dyn.as_ref(), "agent-shared", &persona.memory_blocks);

        match result {
            Err(RuntimeError::SharedBlockRefNotSupported { label, shared_id }) => {
                assert_eq!(label, "shared_notes", "error should name the failing block");
                assert_eq!(
                    shared_id, "mem_01HXYZ_shared",
                    "error should include the shared_id value"
                );
            }
            Ok(()) => panic!("expected SharedBlockRefNotSupported error, got Ok"),
            Err(other) => panic!("expected SharedBlockRefNotSupported, got: {other:?}"),
        }
    }

    // -- Task 14: persona-level KDL rules merge into PolicySet ------------

    #[tokio::test]
    async fn merge_policies_layers_kdl_over_rust_defaults() {
        // Persona declares a KDL Allow rule for `git push*`. The
        // composed PolicySet should evaluate `git push origin main` as
        // Allow (KDL beats RustDefault), while `rm -rf /` still
        // RequireApproval (no KDL rule covers it).
        use pattern_core::{
            EffectCategory, PolicyAction, PolicyContext, PolicyMatcher, PolicyRule, Precedence,
        };

        let persona =
            PersonaSnapshot::new("agent-task14", "T14").with_policy_rules([PolicyRule::new(
                EffectCategory::Shell,
                PolicyMatcher::ShellCommand {
                    pattern: "git push*".into(),
                },
                PolicyAction::Allow,
                Precedence::KdlConfig,
            )]);

        let policies = merge_policies(&persona);

        // KDL Allow wins over the absence of a default for git push.
        assert_eq!(
            policies.evaluate(
                EffectCategory::Shell,
                &PolicyContext::Shell {
                    command: "git push origin main",
                },
            ),
            PolicyAction::Allow,
            "KDL Allow rule should reach the evaluator"
        );

        // Rust default still gates rm -rf — the KDL rule doesn't shadow it.
        match policies.evaluate(
            EffectCategory::Shell,
            &PolicyContext::Shell {
                command: "rm -rf /tmp/x",
            },
        ) {
            PolicyAction::RequireApproval { .. } => {}
            other => panic!("rm -rf should still RequireApproval, got {other:?}"),
        }
    }

    #[test]
    fn merge_policies_with_no_persona_rules_returns_just_defaults() {
        let persona = PersonaSnapshot::new("default-only", "D");
        let policies = merge_policies(&persona);
        // The defaults vec contains five shell rules + one spawn rule.
        assert_eq!(policies.rules().len(), crate::policy::rust_defaults().len());
    }

    /// AC2.2 end-to-end (review fix): a persona with a KDL `Allow`
    /// rule for `git push*` reaches the Shell handler, the policy
    /// evaluates to Allow, and the broker is NOT invoked. Exercises
    /// the full wire `persona.policy_rules → from_persona →
    /// SessionContext.policies → ShellHandler reads cx.user().policies()`.
    #[tokio::test]
    async fn ac2_2_persona_kdl_allow_reaches_shell_handler_and_skips_broker() {
        use crate::sdk::handlers::shell::ShellHandler;
        use crate::sdk::requests::ShellReq;
        use pattern_core::{EffectCategory, PolicyAction, PolicyMatcher, PolicyRule, Precedence};
        use tidepool_effect::EffectHandler;
        use tidepool_repr::DataConTable;

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let db = crate::testing::test_db().await;

        let persona =
            PersonaSnapshot::new("agent-ac2-2", "AC22").with_policy_rules([PolicyRule::new(
                EffectCategory::Shell,
                PolicyMatcher::ShellCommand {
                    pattern: "git push*".into(),
                },
                PolicyAction::Allow,
                Precedence::KdlConfig,
            )]);

        // Wire a real broker + bridge, then watch for any traffic.
        let ctx_owned = SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        );
        let broker = ctx_owned.permission_broker().clone();
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker.clone()));
        let ctx = ctx_owned.with_permission_bridge(bridge);

        let mut rx = broker.subscribe();
        let saw_broker = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_for_thread = saw_broker.clone();
        let watcher = tokio::spawn(async move {
            if rx.recv().await.is_ok() {
                saw_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });

        let result = tokio::task::spawn_blocking(move || {
            let mut h = ShellHandler;
            let table = DataConTable::new();
            let cx_eff = tidepool_effect::EffectContext::with_user(&table, &ctx);
            h.handle(ShellReq::Execute("git push origin main".into()), &cx_eff)
        })
        .await
        .expect("blocking task")
        .expect_err("Phase 1 stub always errors");
        let msg = result.to_string();
        // Allow path: stub error WITHOUT GateApproved marker (gate did
        // not fire because policy returned Allow before any broker call).
        assert!(
            msg.contains("Pattern.Shell.Execute is not implemented"),
            "expected plain stub error, got: {msg}"
        );
        assert!(
            !msg.contains("GateApproved:"),
            "Allow path must not carry GateApproved marker — gate should be skipped, got: {msg}"
        );

        // Allow watcher a beat to record any broker traffic.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            !saw_broker.load(std::sync::atomic::Ordering::SeqCst),
            "broker must NOT receive any request when KDL Allow rule matches"
        );
        watcher.abort();
    }

    /// AC2.3 end-to-end (review fix): a persona with a KDL
    /// `RequireApproval` rule for all file writes (`*` glob) escalates
    /// non-config writes through the broker. Exercises the same wire
    /// as AC2.2 but through the File handler.
    #[tokio::test]
    async fn ac2_3_persona_kdl_require_approval_reaches_file_handler_and_invokes_broker() {
        use crate::sdk::handlers::file::FileHandler;
        use crate::sdk::requests::FileReq;
        use pattern_core::permission::PermissionDecisionKind;
        use pattern_core::{EffectCategory, PolicyAction, PolicyMatcher, PolicyRule, Precedence};
        use tidepool_effect::EffectHandler;
        use tidepool_repr::DataConTable;

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let db = crate::testing::test_db().await;

        let persona =
            PersonaSnapshot::new("agent-ac2-3", "AC23").with_policy_rules([PolicyRule::new(
                EffectCategory::File,
                PolicyMatcher::FilePath {
                    pattern: "*".into(),
                },
                PolicyAction::RequireApproval {
                    reason: Some("all file writes gated for this persona".into()),
                },
                Precedence::KdlConfig,
            )]);

        let ctx_owned = SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        );
        let broker = ctx_owned.permission_broker().clone();
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker.clone()));
        let ctx = ctx_owned.with_permission_bridge(bridge);

        // Subscribe synchronously so the responder never misses.
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::ApproveOnce)
                    .await;
            }
        });

        let result = tokio::task::spawn_blocking(move || {
            let mut h = FileHandler;
            let table = DataConTable::new();
            let cx_eff = tidepool_effect::EffectContext::with_user(&table, &ctx);
            h.handle(
                FileReq::Write("/tmp/notes.txt".into(), "hello".into()),
                &cx_eff,
            )
        })
        .await
        .expect("blocking task")
        .expect_err("Phase 1 stub always errors");
        let msg = result.to_string();
        assert!(
            msg.contains("GateApproved:"),
            "expected GateApproved marker — broker must have observed the prompt and approved, \
             got: {msg}"
        );
        responder.await.unwrap();
    }

    #[test]
    fn from_persona_threads_persona_capabilities_through() {
        use pattern_core::{CapabilitySet, EffectCategory};
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let db = rt.block_on(crate::testing::test_db());

        let persona = PersonaSnapshot::new("caps-thru", "C").with_capabilities(Some(
            CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Message]),
        ));

        let ctx = SessionContext::from_persona(&persona, store, provider, db, rt.handle().clone());
        let caps = ctx.capabilities().expect("persona caps should propagate");
        assert!(caps.contains(EffectCategory::Memory));
        assert!(caps.contains(EffectCategory::Message));
        assert!(!caps.contains(EffectCategory::Shell));
    }
}
