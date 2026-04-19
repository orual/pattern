//! Haskell eval worker — the real [`EvalDispatcher`] backing `code`
//! tool_use invocations.
//!
//! # Design
//!
//! Each session opened for production-path execution spawns one
//! long-lived **eval worker thread** via [`EvalWorker::spawn`]. The
//! thread has a 256 MiB stack (matches tidepool-mcp's convention for
//! GHC-compiled code — the nested continuation frames produced by
//! `do`-notation Haskell easily blow past the default 8 MiB on
//! moderately complex snippets) and owns a small current-thread tokio
//! runtime so the handler `Arc<dyn MemoryStore>` can drive async
//! operations during the sync `compile_and_run` call.
//!
//! Tool calls arrive via `EvalDispatcher::dispatch` → an
//! `tokio::sync::mpsc::UnboundedSender`. For each request the
//! worker:
//!
//! 1. Parses the `code`-tool JSON arguments into `CodeToolInput`.
//! 2. Wraps the snippet in the shared preamble via
//!    `crate::sdk::code_tool::template_source`.
//! 3. Reconstructs a fresh `SdkBundle` — handlers are either unit
//!    structs or `Arc`-wrapped state, so the reconstruction is
//!    effectively free. The fresh `DisplayHandler` is wired to the
//!    session's `TurnSink` (from `pattern_core::traits`) so `Pattern.Display.*` output flows to
//!    the same sink as LLM text chunks.
//! 4. Calls `tidepool_runtime::compile_and_run` against the bundle
//!    with the `SessionContext` as the user value.
//! 5. Sends the `ToolOutcome` (success: JSON payload via
//!    `EvalResult` serialization; error: diagnostic string) back through
//!    a `tokio::sync::oneshot` reply channel.
//!
//! # Runtime shape
//!
//! The worker owns a **multi-thread** tokio runtime (small worker
//! pool, default blocking threads). Single-thread wouldn't work:
//! `MemoryHandler` delegates memory reads/writes to `sqlx`, which
//! issues `tokio::task::spawn_blocking` calls internally on the
//! SQLite path. Those need actual worker threads to run on — a
//! current-thread runtime would deadlock when the handler's sync
//! `compile_and_run` body tries to `block_on` an async sqlx call
//! that itself spawns a blocking task. A multi-thread runtime with
//! modest parallelism (default: `num_cpus`, capped by the runtime
//! builder) sidesteps this cleanly at the cost of a few extra
//! threads per session.
//!
//! Dropping [`EvalWorker`] closes the request channel, which causes
//! the worker to exit cleanly at its next `rx.recv()` iteration; the
//! join handle is awaited in `Drop` with a short timeout (best-effort
//! — if the worker is mid-compile it may outlive the session, which
//! is acceptable since tidepool operations are bounded).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::oneshot;

use pattern_core::types::provider::{ToolCall, ToolOutcome};

use crate::sdk::bundle::SdkBundle;
use crate::sdk::code_tool::{CodeToolInput, template_source};
use crate::sdk::handlers::{
    DisplayHandler, FileHandler, LogHandler, McpHandler, MemoryHandler, MessageHandler,
    RecallHandler, RpcHandler, SearchHandler, ShellHandler, SourcesHandler, SpawnHandler,
    TimeHandler,
};
use crate::session::SessionContext;

use super::EvalDispatcher;

/// One pending eval request: the complete Haskell source (preamble +
/// user code + `result` binding) plus a oneshot reply channel.
struct EvalRequest {
    source: String,
    reply: oneshot::Sender<ToolOutcome>,
}

/// Long-lived Haskell eval worker. One per session.
///
/// See the module-level docs for the design rationale. Holds an
/// `tokio::sync::mpsc::UnboundedSender` to the worker thread + the thread's
/// `std::thread::JoinHandle` (wrapped in `Option` so `Drop` can take it out for
/// the `join` call).
pub struct EvalWorker {
    tx: UnboundedSender<EvalRequest>,
    /// `Option` so `Drop` can move the handle into `join`.
    join_handle: Option<std::thread::JoinHandle<()>>,
    /// Snapshot of the worker's session_id for diagnostics.
    session_id: String,
}

impl std::fmt::Debug for EvalWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvalWorker")
            .field("session_id", &self.session_id)
            .field(
                "worker_alive",
                &self.join_handle.as_ref().is_some_and(|h| !h.is_finished()),
            )
            .finish()
    }
}

impl EvalWorker {
    /// Spawn a new eval worker thread for the given session with a
    /// single include path (typically the Pattern SDK directory).
    ///
    /// Convenience wrapper over [`Self::spawn_with_includes`] for the
    /// common case where the session only needs the SDK tree. If the
    /// session's agents will import `Tidepool.Prelude`,
    /// `Tidepool.Aeson`, etc. — or any code the preamble does —
    /// callers should use [`Self::spawn_with_includes`] and pass
    /// `[sdk_dir, prelude_dir]`.
    pub fn spawn(ctx: Arc<SessionContext>, sdk_dir: PathBuf, session_id: String) -> Self {
        Self::spawn_with_includes(ctx, vec![sdk_dir], session_id)
    }

    /// Spawn a new eval worker thread with multiple GHC include
    /// paths.
    ///
    /// `include_paths` is passed verbatim to
    /// `tidepool_runtime::compile_and_run` on every dispatch. A
    /// typical session wires in two entries:
    ///
    /// 1. Pattern's `haskell/` SDK directory (where `Pattern.Time`
    ///    etc. live). tidepool-extract now bundles the prelude
    ///    internally, so only the SDK dir is required in practice.
    ///
    /// See the module-level docs for the eval loop's design.
    pub fn spawn_with_includes(
        ctx: Arc<SessionContext>,
        include_paths: Vec<PathBuf>,
        session_id: String,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<EvalRequest>();
        let session_id_for_worker = session_id.clone();

        let join_handle = std::thread::Builder::new()
            .name(format!("pattern-eval-worker-{session_id_for_worker}"))
            .stack_size(256 * 1024 * 1024)
            .spawn(move || {
                // Multi-thread runtime — see module docs. Modest
                // worker count since the worker thread itself mostly
                // blocks on GHC compile; the async tasks we run are
                // sqlx operations driven by handlers during eval.
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name(format!("pattern-eval-rt-{session_id_for_worker}"))
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("eval worker failed to build tokio runtime: {e}");
                        return;
                    }
                };

                rt.block_on(async move {
                    while let Some(req) = rx.recv().await {
                        // Wrap the sync eval work in `block_in_place` so
                        // tokio moves other tasks off this worker before
                        // we block it for the duration of the Haskell
                        // compile + JIT run. Without this, effect
                        // handlers inside the JIT that call
                        // `Handle::current().block_on(...)` to drive async
                        // store operations panic with "Cannot start a
                        // runtime from within a runtime" because they
                        // can't block_on the same runtime's worker
                        // they're currently running on. `block_in_place`
                        // is the documented tokio pattern for this.
                        let outcome = tokio::task::block_in_place(|| {
                            run_eval(&req.source, &ctx, &include_paths, &session_id_for_worker)
                        });
                        // Receiver may have dropped (session cancelled
                        // mid-eval) — that's not an error worth
                        // surfacing; just move on to the next request.
                        let _ = req.reply.send(outcome);
                    }
                });
            })
            .expect("failed to spawn eval worker thread");

        Self {
            tx,
            join_handle: Some(join_handle),
            session_id,
        }
    }

    /// `true` when the worker thread has exited (channel closed or
    /// runtime build failure). Exposed for health-checks and tests.
    pub fn is_alive(&self) -> bool {
        self.join_handle.as_ref().is_some_and(|h| !h.is_finished())
    }
}

impl Drop for EvalWorker {
    fn drop(&mut self) {
        // Dropping `tx` closes the channel; the worker's `rx.recv()`
        // returns `None`, exits the loop, drops the tokio runtime, and
        // the thread returns. Join best-effort — if a compile is in
        // flight we let the thread outlive the session rather than
        // blocking the caller indefinitely. compile_and_run is
        // bounded by tidepool's own timeout, so the worker will
        // terminate soon regardless.
        if let Some(handle) = self.join_handle.take() {
            // Drop sender explicitly to make the intent clear.
            drop(std::mem::replace(&mut self.tx, mpsc::unbounded_channel().0));
            // Don't join — session teardown shouldn't block on a
            // potentially-in-flight Haskell compile. The thread will
            // terminate when its compile finishes + it sees the
            // closed channel. Detach by letting the handle drop.
            drop(handle);
        }
    }
}

#[async_trait]
impl EvalDispatcher for EvalWorker {
    async fn dispatch(&self, tool_call: ToolCall, preamble: &str) -> ToolOutcome {
        // 1. Parse the code-tool JSON arguments.
        let params = match serde_json::from_value::<CodeToolInput>(tool_call.fn_arguments.clone()) {
            Ok(p) => p,
            Err(e) => {
                return ToolOutcome::Error(format!(
                    "invalid code tool arguments for call {}: {e}",
                    tool_call.call_id
                ));
            }
        };

        // 2. Template the source.
        let source = template_source(
            preamble,
            &params.code,
            params.imports.as_deref(),
            params.helpers.as_deref(),
        );

        // 3. Send to worker, await reply.
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = EvalRequest {
            source,
            reply: reply_tx,
        };
        if self.tx.send(request).is_err() {
            return ToolOutcome::Error(
                "eval worker channel closed — session shutting down or worker crashed".into(),
            );
        }
        match reply_rx.await {
            Ok(outcome) => outcome,
            Err(_) => ToolOutcome::Error(
                "eval worker dropped reply channel — the evaluation was abandoned".into(),
            ),
        }
    }
}

/// Inner eval: build a fresh bundle, compile+run the source, render
/// the result. Called synchronously on the worker thread inside the
/// worker's tokio runtime.
fn run_eval(
    source: &str,
    ctx: &Arc<SessionContext>,
    include_paths: &[PathBuf],
    session_id: &str,
) -> ToolOutcome {
    // Bundle construction is cheap: 10/13 handlers are unit structs
    // or Arc-wrapped singletons. Fresh DisplayHandler per eval that
    // forwards to the session's TurnSink, so `Pattern.Display.*`
    // output reaches the same sink as LLM text.
    let display = DisplayHandler::new();
    display.forward_to_turn_sink(ctx.turn_sink().clone());

    let store = ctx.memory_store();
    let mut bundle: SdkBundle = frunk::hlist![
        MemoryHandler::new(store.clone()),
        SearchHandler::new(store.clone()),
        RecallHandler::new(store),
        MessageHandler,
        display,
        TimeHandler,
        LogHandler::for_session(session_id.to_string()),
        ShellHandler,
        FileHandler,
        SourcesHandler,
        McpHandler,
        RpcHandler,
        SpawnHandler,
    ];

    // Coerce the owned PathBufs into the &[&Path] slice
    // compile_and_run expects.
    let include_refs: Vec<&std::path::Path> = include_paths.iter().map(|p| p.as_path()).collect();

    match tidepool_runtime::compile_and_run(
        source,
        "result",
        &include_refs,
        &mut bundle,
        ctx.as_ref(),
    ) {
        Ok(eval_result) => {
            // Convert the evaluated Value to JSON via tidepool's
            // renderer (which knows the DataConTable for proper
            // constructor-name rendering).
            ToolOutcome::Success(eval_result.to_json())
        }
        Err(e) => ToolOutcome::Error(format!("haskell eval failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SdkLocation;
    use crate::testing::{InMemoryMemoryStore, NopProviderClient};
    use pattern_core::ProviderClient;
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::snapshot::PersonaSnapshot;

    async fn test_ctx() -> (Arc<SessionContext>, PathBuf) {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        let ctx = Arc::new(SessionContext::from_persona(&persona, store, provider, db));
        let sdk_dir = SdkLocation::default()
            .resolve()
            .expect("SDK dir should resolve for tests");
        (ctx, sdk_dir)
    }

    /// Worker lifecycle: spawn, drop, thread terminates.
    ///
    /// Gated on preflight — skips cleanly when tidepool-extract is
    /// not available.
    #[tokio::test]
    async fn worker_spawns_and_drops_cleanly() {
        if crate::preflight::check().is_err() {
            return;
        }
        let (ctx, sdk_dir) = test_ctx().await;
        let worker = EvalWorker::spawn(ctx, sdk_dir, "test-session".into());
        assert!(
            worker.is_alive(),
            "worker should be alive immediately after spawn"
        );
        drop(worker);
        // If Drop hangs this test hangs — we don't join, so it must
        // return quickly. Nothing more to assert here.
    }

    /// Dispatching malformed JSON args yields an Error outcome
    /// WITHOUT the request ever reaching the worker thread (no GHC
    /// compile triggered).
    #[tokio::test]
    async fn dispatch_with_invalid_arguments_returns_error_outcome() {
        if crate::preflight::check().is_err() {
            return;
        }
        let (ctx, sdk_dir) = test_ctx().await;
        let worker = EvalWorker::spawn(ctx, sdk_dir, "test-session".into());

        let bad_call = ToolCall {
            call_id: "toolu_1".into(),
            fn_name: "code".into(),
            // Missing required `code` field.
            fn_arguments: serde_json::json!({"not_code_field": "oops"}),
            thought_signatures: None,
            thought_signatures_provenance: None,
        };
        let outcome = worker.dispatch(bad_call, "").await;
        match outcome {
            ToolOutcome::Error(msg) => {
                assert!(
                    msg.contains("invalid code tool arguments"),
                    "expected argument-parsing error, got: {msg}"
                );
            }
            other => panic!("expected Error outcome, got {other:?}"),
        }
    }

    /// End-to-end: a real Haskell snippet compiles + runs via the
    /// worker and returns a Success outcome with the expected JSON
    /// payload.
    ///
    /// # Environment requirements
    ///
    /// Gated only on `tidepool-extract` being available (via
    /// `preflight::check`). tidepool-extract bundles the prelude
    /// internally — no external lib directory required.
    ///
    /// The first run absorbs GHC warm-up (~seconds on cold cache,
    /// ~ms on warm).
    #[tokio::test]
    async fn dispatch_evaluates_trivial_haskell_snippet_end_to_end() {
        if crate::preflight::check().is_err() {
            return;
        }
        let (ctx, sdk_dir) = test_ctx().await;
        let session_id = "e2e-test".to_string();
        let worker = EvalWorker::spawn_with_includes(ctx, vec![sdk_dir], session_id);
        let preamble = crate::sdk::preamble::build(&crate::sdk::bundle::canonical_effect_decls());

        let tc = ToolCall {
            call_id: "toolu_42".into(),
            fn_name: "code".into(),
            // The `code` tool wraps the snippet in
            // `result = do {...; paginateResult 4096 (toJSON _r)}`.
            // A trivial body that returns an Int value.
            fn_arguments: serde_json::json!({
                "code": "pure (42 :: Int)"
            }),
            thought_signatures: None,
            thought_signatures_provenance: None,
        };
        let outcome = worker.dispatch(tc, &preamble).await;
        match outcome {
            ToolOutcome::Success(v) => {
                // Paginated result wraps the value; the exact shape
                // is defined by tidepool-mcp's paginateResult, but it
                // should be non-null and contain 42 somewhere in its
                // string form.
                let s = v.to_string();
                assert!(
                    s.contains("42"),
                    "expected rendered JSON to contain 42, got: {s}"
                );
            }
            ToolOutcome::Error(msg) => {
                panic!("expected Success, got Error: {msg}");
            }
        }
    }

    /// Dispatching after the worker has been dropped yields an Error
    /// outcome naming the closed channel.
    #[tokio::test]
    async fn dispatch_after_worker_drop_returns_error_outcome() {
        // Don't gate on preflight — we drop before needing tidepool.
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        let ctx = Arc::new(SessionContext::from_persona(&persona, store, provider, db));

        // Stub sdk_dir — we never actually hit the worker thread.
        let worker = EvalWorker {
            tx: {
                let (tx, _rx) = mpsc::unbounded_channel::<EvalRequest>();
                tx
            },
            join_handle: None,
            session_id: "stub".into(),
        };
        drop(worker.tx.clone()); // doesn't close — we have the original
        // Force close by dropping the receiver-holding worker.
        // Actually, explicit: create a worker whose tx leads to a
        // dropped receiver. Simulate by dropping the receiver
        // manually via a one-off channel.
        let (dead_tx, dead_rx) = mpsc::unbounded_channel::<EvalRequest>();
        drop(dead_rx);
        let dead_worker = EvalWorker {
            tx: dead_tx,
            join_handle: None,
            session_id: "stub".into(),
        };

        let tc = ToolCall {
            call_id: "toolu_1".into(),
            fn_name: "code".into(),
            fn_arguments: serde_json::json!({"code": "pure ()"}),
            thought_signatures: None,
            thought_signatures_provenance: None,
        };
        let outcome = dead_worker.dispatch(tc, "").await;
        match outcome {
            ToolOutcome::Error(msg) => {
                assert!(
                    msg.contains("channel closed"),
                    "expected channel-closed error, got: {msg}"
                );
            }
            other => panic!("expected Error outcome, got {other:?}"),
        }
        drop(ctx);
    }
}
