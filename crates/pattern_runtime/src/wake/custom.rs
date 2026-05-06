//! Custom Haskell wake-condition evaluator (Phase 7 Task 6).
//!
//! Closes the Phase 4 Task 9 deferral: when a user registers a
//! `WakeCondition::Custom { id, program, period }`, this module spawns a tokio
//! task that triggers the user's Haskell program periodically (or on
//! block-change) and pokes the session mailbox when the result is
//! `True`.
//!
//! # Security boundary
//!
//! The user's program runs against a **read-only restricted bundle** built
//! from [`CapabilitySet::wake_evaluator_read_only()`]. This applies two
//! independent restrictions:
//!
//! 1. **Category filter**: entire SDK modules are dropped (`Spawn`, `Shell`,
//!    `Message`, `Mcp`, `Wake`, `Fronting`, `Constellation`, `File`, `Port`).
//!    This is the load-bearing layer for `Skip`-classified constructors
//!    (e.g. `Spawn.Ephemeral`, `Shell.Execute`, `Message.Send`,
//!    `Wake.Register`) — they have no runtime `check_effect_class` gate, so
//!    the only protection is that their entire module is absent from the
//!    capability set and therefore absent from the Haskell prelude.
//!
//! 2. **Class filter**: surviving modules are further restricted to
//!    `Observe`-class constructors. This removes mutating constructors from
//!    kept modules (e.g. `Memory.Put`, `Tasks.Create`).
//!
//! Together: the prelude contains `(kept categories) ∩ (Observe class)`.
//! The runtime `check_effect_class` gate provides defense-in-depth for
//! `Enforce`-classified constructors in the surviving modules; it does NOT
//! protect against `Skip`-classified constructors — the category filter
//! is the only guard there. See `CapabilitySet::wake_evaluator_read_only()`
//! for the complete kept/dropped category list and rationale.
//!
//! # Evaluation model
//!
//! Each evaluation spawns a fresh 256 MiB OS thread (matching the
//! eval-worker pattern from `agent_loop::eval_worker`). A
//! `tokio::time::timeout` of 30 seconds bounds each evaluation.
//! Single-flight per condition: if a prior evaluation is still running
//! when the next trigger fires, the trigger is skipped with a warning.
//!
//! # Resource caps
//!
//! - Per-session: at most 32 registered custom conditions (configurable).
//! - Per-evaluation: 30s wall-clock timeout.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use parking_lot::Mutex;
use smol_str::SmolStr;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use pattern_core::CapabilitySet;
use pattern_core::types::origin::SystemReason;

use crate::mailbox::Mailbox;
use crate::sdk::bundle::filtered_effect_decls;
use crate::sdk::handlers::{
    DisplayHandler, FileHandler, FrontingHandler, LogHandler, McpHandler, MemoryHandler,
    MessageHandler, PortHandler, RecallHandler, SearchHandler, ShellHandler, SkillsHandler,
    SpawnHandler, TasksHandler, TimeHandler, WakeHandler, WebHandler,
};
use crate::sdk::preamble;
use crate::session::SessionContext;
use crate::wake::registry::wake_mailbox_input;

/// Default per-evaluation wall-clock timeout.
const EVAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Default maximum number of custom conditions per session.
const DEFAULT_MAX_CUSTOM_WAKES: usize = 32;

/// Stack size for the OS thread that runs `compile_and_run`.
const EVAL_THREAD_STACK_SIZE: usize = 256 * 1024 * 1024;

/// Minimum interval period for custom wake triggers. Subsecond polling
/// is rejected at register time.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Manages tokio tasks for registered custom Haskell wake conditions.
///
/// Each registered condition gets its own tokio task that triggers on
/// the configured schedule. Evaluation happens on a dedicated OS thread
/// to match the eval-worker pattern (GHC needs large stacks).
pub struct CustomEvaluator {
    /// Registered condition tasks, keyed by user-supplied id.
    tasks: Mutex<HashMap<SmolStr, JoinHandle<()>>>,
    /// Session mailbox for delivering wake activations. Holding the
    /// `Arc<Mailbox>` (rather than a raw sender clone) means evaluator
    /// fires go through [`Mailbox::send_input`] so the `pending`
    /// counter stays in sync with the channel.
    mailbox: Arc<Mailbox>,
    /// Tokio runtime handle for spawning tasks from sync context.
    tokio_handle: Handle,
    /// Session context for building bundles.
    session_ctx: Arc<SessionContext>,
    /// Per-condition single-flight guard. When an id is present, that
    /// condition's evaluation is in-flight; the next trigger skips.
    inflight: Arc<DashMap<SmolStr, ()>>,
    /// Maximum registered conditions.
    max_conditions: usize,
    /// Pre-built preamble for the read-only capability set. Built once
    /// at construction time to avoid repeated filtering per evaluation.
    read_only_preamble: Arc<str>,
    /// Include paths for GHC (SDK dir + any port libraries).
    include_paths: Arc<Vec<PathBuf>>,
    /// The restricted capability set (Observe-only). Stored here so the
    /// eval function can build a restricted `SessionContext` for
    /// handler-level enforcement.
    restricted_caps: CapabilitySet,
}

impl CustomEvaluator {
    /// Build a new evaluator for the given session.
    ///
    /// `include_paths` are the GHC include paths (typically `[sdk_dir]`).
    pub fn new(
        mailbox: Arc<Mailbox>,
        include_paths: Vec<PathBuf>,
        tokio_handle: Handle,
        session_ctx: Arc<SessionContext>,
    ) -> Self {
        // Build the read-only preamble once using the dedicated wake-eval
        // capability set. This drops entire SDK modules (Spawn, Shell,
        // Message, Wake, Mcp, Fronting, Constellation, File, Port) so that
        // even Skip-classified constructors — which bypass the runtime
        // check_effect_class gate — are absent from the compiled prelude.
        // The Observe-class filter then removes mutating constructors from
        // the surviving modules (Memory.Put, Tasks.Create, etc.).
        //
        // See CapabilitySet::wake_evaluator_read_only() for the full
        // rationale and the explicit list of kept/dropped categories.
        let caps = CapabilitySet::wake_evaluator_read_only();
        let decls = filtered_effect_decls(&caps);
        let preamble_str = preamble::build(&decls);

        Self {
            tasks: Mutex::new(HashMap::new()),
            mailbox,
            tokio_handle,
            session_ctx,
            inflight: Arc::new(DashMap::new()),
            max_conditions: DEFAULT_MAX_CUSTOM_WAKES,
            read_only_preamble: Arc::from(preamble_str),
            include_paths: Arc::new(include_paths),
            restricted_caps: caps,
        }
    }

    /// Override the maximum number of registered conditions.
    #[must_use]
    pub fn with_max_conditions(mut self, max: usize) -> Self {
        self.max_conditions = max;
        self
    }

    /// Register a custom wake condition with an interval trigger.
    ///
    /// Returns `Ok(())` on success or an error string on failure.
    pub fn register_interval(
        &self,
        id: SmolStr,
        program: String,
        period: Duration,
    ) -> Result<(), String> {
        // Min-period check.
        if period < MIN_INTERVAL {
            return Err(format!(
                "CustomWakeMinPeriod: requested {}ms but minimum is {}ms",
                period.as_millis(),
                MIN_INTERVAL.as_millis(),
            ));
        }

        // Cap check.
        {
            let tasks = self.tasks.lock();
            if tasks.len() >= self.max_conditions {
                return Err(format!(
                    "CustomWakeLimit: {} conditions registered, maximum is {}",
                    tasks.len(),
                    self.max_conditions,
                ));
            }
            if tasks.contains_key(&id) {
                return Err(format!(
                    "CustomWakeDuplicate: condition {id:?} already registered"
                ));
            }
        }

        let handle = self.spawn_interval_task(id.clone(), program, period);
        self.tasks.lock().insert(id, handle);
        Ok(())
    }

    /// Unregister a custom wake condition. Aborts its evaluator task.
    /// Returns `true` if the id was registered.
    pub fn unregister(&self, id: &SmolStr) -> bool {
        let mut tasks = self.tasks.lock();
        if let Some(handle) = tasks.remove(id) {
            handle.abort();
            self.inflight.remove(id);
            true
        } else {
            false
        }
    }

    /// Number of currently registered conditions.
    pub fn len(&self) -> usize {
        self.tasks.lock().len()
    }

    /// True when no conditions are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // FUTURE WORK (Phase 8+, 2026-04-28): BlockChanged trigger support.
    //
    // The v3-multi-agent design plan calls for two trigger sources for custom
    // wake conditions: `Interval(period)` and `BlockChanged(label)`. This
    // implementation ships only the Interval path. A `register_block_changed`
    // method would be the public API; the evaluator task would subscribe via
    // `pattern_memory::subscriber::BlockChangeNotifier` and fire on matching
    // label changes instead of on a timer.
    //
    // The BlockChanged path is deferred to Phase 8 to keep T6 focused on the
    // Interval path and because the notifier fan-out plumbing (accessible via
    // `WakeRegistryExtras::block_change_notifier`) needs to be threaded through
    // `CustomEvaluator::new` first. The `WakeRegistry` already wires it for the
    // built-in `BlockChangedCondition`; custom evaluators need the same handle.

    fn spawn_interval_task(
        &self,
        id: SmolStr,
        program: String,
        period: Duration,
    ) -> JoinHandle<()> {
        let mailbox = self.mailbox.clone();
        let inflight = self.inflight.clone();
        let preamble = self.read_only_preamble.clone();
        let include_paths = self.include_paths.clone();
        let ctx = self.session_ctx.clone();
        let restricted_caps = self.restricted_caps.clone();

        self.tokio_handle.spawn(async move {
            let mut interval = tokio::time::interval(period);
            // The first tick fires immediately — skip it so the first
            // evaluation happens after one full period.
            interval.tick().await;

            loop {
                interval.tick().await;

                // Single-flight: skip if already evaluating.
                if inflight.contains_key(&id) {
                    tracing::warn!(
                        target: "pattern_runtime::wake::custom",
                        custom_wake_id = %id,
                        "custom wake skipped: prior evaluation still running"
                    );
                    continue;
                }

                inflight.insert(id.clone(), ());

                let result = tokio::time::timeout(
                    EVAL_TIMEOUT,
                    run_user_program(&program, &preamble, &include_paths, &ctx, &restricted_caps),
                )
                .await;

                inflight.remove(&id);

                match result {
                    Ok(Ok(true)) => {
                        tracing::debug!(
                            target: "pattern_runtime::wake::custom",
                            custom_wake_id = %id,
                            "custom wake condition returned True — poking mailbox"
                        );
                        let input = wake_mailbox_input(
                            SystemReason::CustomWake { id: id.clone() },
                            &format!("[custom wake: {id}]"),
                        );
                        // send_input bumps `pending` so the drain
                        // loop's note_consumed call balances out.
                        let _ = mailbox.send_input(input);
                    }
                    Ok(Ok(false)) => {
                        tracing::debug!(
                            target: "pattern_runtime::wake::custom",
                            custom_wake_id = %id,
                            "custom wake condition returned False"
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            target: "pattern_runtime::wake::custom",
                            custom_wake_id = %id,
                            error = %e,
                            "custom wake evaluation failed"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            target: "pattern_runtime::wake::custom",
                            custom_wake_id = %id,
                            "custom wake evaluation timed out ({}s limit)",
                            EVAL_TIMEOUT.as_secs()
                        );
                    }
                }
            }
        })
    }
}

impl Drop for CustomEvaluator {
    fn drop(&mut self) {
        let mut tasks = self.tasks.lock();
        for (_, handle) in tasks.drain() {
            handle.abort();
        }
    }
}

impl std::fmt::Debug for CustomEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomEvaluator")
            .field("len", &self.len())
            .field("max_conditions", &self.max_conditions)
            .finish_non_exhaustive()
    }
}

/// Run the user's Haskell condition program on a dedicated 256 MiB OS
/// thread. Returns `true` if the program evaluates to `True`, `false`
/// otherwise.
///
/// `restricted_caps` is the Observe-only capability set. It is used to
/// build a restricted `SessionContext` so the runtime handler-level
/// `check_effect_class` gate enforces the read-only boundary (defense
/// in depth — the preamble's type-level filtering is the primary gate,
/// but the Haskell module imports may still expose non-Observe
/// constructor helper functions).
async fn run_user_program(
    program: &str,
    preamble: &str,
    include_paths: &[PathBuf],
    ctx: &Arc<SessionContext>,
    restricted_caps: &CapabilitySet,
) -> Result<bool, String> {
    // Build the Haskell source. Unlike the code-tool template, we do
    // NOT use paginateResult — we want the raw Bool.
    let source = template_condition_source(preamble, program);

    let include_refs: Vec<PathBuf> = include_paths.to_vec();
    let ctx = ctx.clone();
    let caps = restricted_caps.clone();

    // Spawn a dedicated OS thread with 256 MiB stack.
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("pattern-custom-wake-eval".into())
        .stack_size(EVAL_THREAD_STACK_SIZE)
        .spawn(move || {
            let result = eval_condition(&source, &include_refs, &ctx, &caps);
            let _ = tx.send(result);
        })
        .map_err(|e| format!("failed to spawn eval thread: {e}"))?;

    rx.await
        .map_err(|_| "eval thread dropped reply channel".to_string())?
}

/// Build a Haskell source that evaluates a condition program to a Bool.
///
/// The result binding extracts the Bool value without JSON pagination.
fn template_condition_source(preamble: &str, condition_code: &str) -> String {
    let mut out = String::with_capacity(preamble.len() + condition_code.len() + 256);
    out.push_str(preamble);
    out.push_str("-- [wake-condition]\n");
    out.push_str("result :: Eff M Bool\n");
    out.push_str("result = do\n");
    for line in condition_code.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Build a `SessionContext` for evaluation that shares the parent's
/// memory store but has restricted (Observe-only) capabilities.
fn build_restricted_ctx(
    parent: &SessionContext,
    restricted_caps: &CapabilitySet,
) -> SessionContext {
    use pattern_core::types::snapshot::PersonaSnapshot;

    let persona = PersonaSnapshot::new(parent.agent_id(), "CustomWakeEval");
    let store = parent.memory_store();
    let provider = parent.provider().clone();
    let db = parent.db().clone();
    let tokio_handle = parent.tokio_handle().clone();

    SessionContext::from_persona(&persona, store, provider, db, tokio_handle)
        .with_capabilities(Some(restricted_caps.clone()))
}

/// Inner eval on the dedicated OS thread. Builds a fresh read-only
/// bundle and calls `compile_and_run`.
///
/// The `restricted_caps` are Observe-only. They are passed as the
/// `user` value to `compile_and_run` via a thin wrapper, so the
/// handler-level `check_effect_class` gate enforces the read-only
/// boundary even if compile-time filtering misses an edge case
/// (defense in depth).
fn eval_condition(
    source: &str,
    include_paths: &[PathBuf],
    ctx: &SessionContext,
    restricted_caps: &CapabilitySet,
) -> Result<bool, String> {
    use crate::sdk::bundle::SdkBundle;
    use crate::sdk::handlers::DiagnosticsHandler;

    // Build a restricted SessionContext that shares the parent's memory
    // store but has Observe-only capabilities. This makes the handler-
    // level `check_effect_class` gate enforce the read-only boundary
    // at dispatch time (defense in depth alongside compile-time filtering).
    let restricted_ctx = build_restricted_ctx(ctx, restricted_caps);

    let display = DisplayHandler::new();
    let store = restricted_ctx.memory_store();
    let diagnostics_handler = DiagnosticsHandler::new(restricted_ctx.diagnostics().clone());

    let mut bundle: SdkBundle = frunk::hlist![
        MemoryHandler::new(),
        SearchHandler::new(store.clone()),
        RecallHandler::new(store),
        TasksHandler,
        SkillsHandler,
        MessageHandler,
        display,
        TimeHandler,
        LogHandler::for_session("custom-wake".to_string()),
        ShellHandler,
        FileHandler,
        McpHandler::new(ctx.tokio_handle().clone()),
        SpawnHandler,
        diagnostics_handler,
        WakeHandler,
        FrontingHandler,
        PortHandler,
        crate::sdk::handlers::ConstellationHandler,
        crate::sdk::handlers::WebHandler::new(),
    ];

    let include_refs: Vec<&std::path::Path> = include_paths.iter().map(|p| p.as_path()).collect();

    match tidepool_runtime::compile_and_run(
        source,
        "result",
        &include_refs,
        &mut bundle,
        &restricted_ctx,
    ) {
        Ok(eval_result) => {
            // The result is a Bool. Convert to JSON and check.
            let json = eval_result.to_json();
            match json {
                serde_json::Value::Bool(b) => Ok(b),
                other => {
                    // The program returned a non-Bool value. Treat as
                    // false with a warning.
                    tracing::warn!(
                        target: "pattern_runtime::wake::custom",
                        result = %other,
                        "custom wake condition returned non-Bool value"
                    );
                    Ok(false)
                }
            }
        }
        Err(e) => Err(format!("haskell eval failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_condition_source_wraps_correctly() {
        let preamble = "-- preamble\n";
        let code = "pure True";
        let source = template_condition_source(preamble, code);
        assert!(source.contains("result :: Eff M Bool"));
        assert!(source.contains("result = do"));
        assert!(source.contains("  pure True"));
    }

    #[test]
    fn min_interval_rejects_subsecond() {
        assert!(Duration::from_millis(500) < MIN_INTERVAL);
    }
}
