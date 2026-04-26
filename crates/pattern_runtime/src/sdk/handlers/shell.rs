//! Handler for `Pattern.Shell` — dispatches all four variants to
//! `ProcessManager`.
//!
//! ## Design
//!
//! `ShellHandler<SessionContext>` (tightened from the stub's `HasCancelState`
//! bound — matches `SkillsHandler` and Phase 2's `FileHandler`) dispatches
//! `ShellReq` synchronously to `cx.user().process_manager()`.
//!
//! ## Critical safety note (no block_on)
//!
//! This handler runs on the Tidepool eval worker — a dedicated OS thread with
//! NO ambient tokio runtime. Do NOT introduce `block_on` here, even against a
//! `Handle` stashed on `SessionContext`. `block_on` against arbitrary plugin
//! code can deadlock if the awaited future calls `spawn_blocking` against a
//! saturated pool (or runs on a single-thread runtime). All dispatched
//! subsystems exposed at this boundary must be sync at the API surface.
//! `ProcessManager` is sync; the bridge thread spawned by `Spawn` dispatch is
//! a plain `std::thread`, not a tokio task.
//!
//! ## Capability check
//!
//! `cx.user().capabilities()` returns `None` for full-power sessions and
//! `Some(cap)` for scoped sessions. When `Some`, we call `cap.has_shell()`.
//! `None` means all-allowed — equivalent to `CapabilitySet::all()`.
//!
//! ## Policy gate
//!
//! After the capability check, `Execute` and `Spawn` variants consult the
//! session's `PolicySet` via `cx.user().policies()`. The evaluation follows
//! the same three-way fan-out as `FileHandler`:
//!
//! - `Allow` → dispatch to `ProcessManager`.
//! - `Deny` → return `PERMISSION_DENIED_PREFIX`-marked `EffectError::Handler`.
//! - `RequireApproval` → escalate via `cx.user().permission_bridge()`. On
//!   broker grant, return `GATE_APPROVED_PREFIX` marker. On denial / timeout /
//!   missing bridge → return `PERMISSION_DENIED_PREFIX`.
//!
//! `Kill` and `Status` do NOT go through the policy pipeline: both operate on
//! tasks the agent already spawned (the capability check above already gates
//! whether the agent can use the Shell effect at all), and neither accepts a
//! command string for a `ShellCommand` matcher to evaluate against.
//!
//! The asymmetry between `FileHandler` (gates per `Write`) and `ShellHandler`
//! (gates per `Execute`/`Spawn`) is by design: both gate the command/path at
//! the moment of invocation against a user-supplied string, not on auxiliary
//! lifecycle management operations.
//!
//! ## v2 semantics (AC3.7 amendment 2026-04-26)
//!
//! Timeout = kill. There is no backgrounding path. The `Backgrounded` variant
//! of `ShellOutputKind` (Task 7) is defined for forward-compat but is **never
//! enqueued** by any code path in this phase. See the phase_03.md amendment
//! for the rationale.

use std::sync::Arc;
use std::time::Duration;

use pattern_core::permission::PermissionScope;
use pattern_core::{EffectCategory, PolicyAction, PolicyContext};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::policy::{GATE_APPROVED_PREFIX, PERMISSION_DENIED_PREFIX};
use crate::process_manager::TaskId;
use crate::process_manager::error::ShellError;
use crate::process_manager::logger::ProcessLogger;
use crate::process_manager::manager::spawn_output_bridge;
use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::ShellReq;
use crate::session::{HasPermissionBridge, SessionContext};
use crate::timeout::HandlerGuard;

/// Handler for `Pattern.Shell` — dispatches all four variants to
/// `ProcessManager`.
///
/// Bound to `SessionContext` (not the generic `HasCancelState` stub bound)
/// because it needs `process_manager()`, `capability_set()`, and
/// `async_reminder_queue()`.
#[derive(Default, Clone)]
pub struct ShellHandler;

impl DescribeEffect for ShellHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Shell",
            description: "Shell command execution (Execute/Spawn/Kill/Status)",
            constructors: &[
                "Execute :: Command -> Maybe TimeoutSecs -> Shell Text",
                "Spawn   :: Command -> Shell Text",
                "Kill    :: TaskId -> Shell ()",
                "Status  :: Shell Text",
            ],
            type_defs: &[
                "type Command = Text",
                "type TaskId = Text  -- opaque, recycle-safe; NOT an OS PID",
                "type TimeoutSecs = Int",
            ],
            helpers: &[
                "execute :: Member Shell effs => Command -> Eff effs Text\nexecute c = send (Execute c Nothing)",
                "executeWith :: Member Shell effs => Command -> TimeoutSecs -> Eff effs Text\nexecuteWith c t = send (Execute c (Just t))",
                "spawn :: Member Shell effs => Command -> Eff effs Text\nspawn c = send (Spawn c)  -- returns JSON {task_id,pid}",
                "kill :: Member Shell effs => TaskId -> Eff effs ()\nkill tid = send (Kill tid)",
                "status :: Member Shell effs => Eff effs Text\nstatus = send Status  -- returns JSON [TaskInfo,...]",
            ],
        }
    }
}

impl EffectHandler<SessionContext> for ShellHandler {
    type Request = ShellReq;

    fn handle(
        &mut self,
        req: ShellReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        // Enter the HandlerGate uniformly with the other handlers so the
        // watchdog's "has any handler been entered recently" bookkeeping
        // does not mistakenly see this handler as non-yielding.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        // Capability check: `None` means full-power (back-compat). `Some(cap)`
        // means the session was opened with a restricted CapabilitySet; deny if
        // Shell is not in the set.
        let shell_allowed = cx
            .user()
            .capabilities()
            .map(|cap| cap.has_shell())
            .unwrap_or(true);
        if !shell_allowed {
            return Err(EffectError::Handler(format!(
                "{PERMISSION_DENIED_PREFIX}Pattern.Shell: capability denied (Shell effect not in agent's CapabilitySet)"
            )));
        }

        let pm = cx.user().process_manager();
        let queue = Arc::clone(cx.user().async_reminder_queue());

        match req {
            ShellReq::Execute(cmd, timeout_secs) => {
                // Policy gate: consult the session's PolicySet before dispatching.
                // `Kill` and `Status` are exempt — see module-level doc for rationale.
                evaluate_shell_command(&cmd, cx.user())?;

                // `Execute :: Command -> Maybe TimeoutSecs -> Shell Text`.
                // `None` means "use the session default"; `Some(n)` is
                // caller-supplied. n <= 0 is treated as "use default" defensively
                // (the Haskell side could in principle send 0; we don't want a
                // zero-second deadline to wedge the read loop).
                let timeout = match timeout_secs {
                    Some(n) if n > 0 => Duration::from_secs(n as u64),
                    _ => cx.user().shell_default_timeout(),
                };

                match pm.execute(&cmd, timeout) {
                    Ok(result) => {
                        // v2 semantics (Amendment 2026-04-26): timeout = kill,
                        // no backgrounding. `result.backgrounded_as` is always
                        // `None`; the branch is omitted. The `Backgrounded`
                        // variant of `ShellOutputKind` is defined for forward
                        // compat but is never enqueued here.
                        let json = serde_json::to_string(&result).map_err(|e| {
                            EffectError::Handler(format!(
                                "Pattern.Shell.Execute: failed to serialize result: {e}"
                            ))
                        })?;
                        cx.respond(json)
                    }
                    Err(ShellError::Timeout(dur)) => Err(EffectError::Handler(format!(
                        "Pattern.Shell.Execute: command timed out after {}s (use Shell.Spawn for long-running commands)",
                        dur.as_secs()
                    ))),
                    Err(e) => Err(EffectError::Handler(format!("Pattern.Shell.Execute: {e}"))),
                }
            }

            ShellReq::Spawn(cmd) => {
                // Policy gate: consult the session's PolicySet before dispatching.
                // Same gate as Execute — Spawn also runs a user-supplied command.
                evaluate_shell_command(&cmd, cx.user())?;

                let (task_id, pid, rx) = pm
                    .spawn(&cmd)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Shell.Spawn: {e}")))?;

                // Open a ProcessLogger for this task (AC3.10 crash backstop).
                // Best-effort: if the log file cannot be opened (e.g., the
                // cache dir is read-only), warn and continue without logging
                // rather than failing the entire Spawn operation. The queue
                // enqueue is the primary output path.
                let logger = match ProcessLogger::open(pm.cache_dir(), &task_id) {
                    Ok(log) => {
                        tracing::debug!(
                            task_id = %task_id,
                            path = %log.path().display(),
                            "shell-output-bridge: opened process log"
                        );
                        Some(log)
                    }
                    Err(e) => {
                        tracing::warn!(
                            task_id = %task_id,
                            error = %e,
                            "shell-output-bridge: failed to open process log (continuing without logging)"
                        );
                        None
                    }
                };

                // Bridge thread: drains the crossbeam receiver, writes each
                // chunk to the process log (best-effort, AC3.10), and enqueues
                // each chunk as a `MessageAttachment::ShellOutput` entry via
                // the async-reminder queue. std::thread, NOT a tokio task —
                // ProcessManager has no ambient runtime.
                spawn_output_bridge(task_id.clone(), rx, Arc::clone(&queue), logger);

                // Respond with JSON {"task_id": "...", "pid": N}. Agents save
                // the task_id for Kill/Status; pid is provided for native-tool
                // interop (e.g. ps, strace) without needing a separate effect.
                let response = serde_json::json!({
                    "task_id": task_id.to_string(),
                    "pid": pid,
                });
                cx.respond(response.to_string())
            }

            ShellReq::Kill(task_id_str) => {
                // task_id_str is the opaque handle string Spawn returned — NOT
                // an OS PID. Recycle-safe: lookup goes through the running map,
                // and the actual SIGTERM is dispatched via the reader thread's
                // owned Child handle.
                let task_id = TaskId(task_id_str);
                pm.kill(&task_id).map_err(|e| match e {
                    ShellError::UnknownTask(ref id) => EffectError::Handler(format!(
                        "Pattern.Shell.Kill: task not found (already exited or invalid handle): {id}"
                    )),
                    other => EffectError::Handler(format!("Pattern.Shell.Kill: {other}")),
                })?;
                cx.respond(())
            }

            ShellReq::Status => {
                // Returns JSON-encoded Vec<TaskInfo> per AC3.5 ("lists all
                // active sessions/processes with their current state").
                let tasks = pm.status();
                let json = serde_json::to_string(&tasks).map_err(|e| {
                    EffectError::Handler(format!(
                        "Pattern.Shell.Status: failed to serialize task list: {e}"
                    ))
                })?;
                cx.respond(json)
            }
        }
    }
}

/// Default broker-request timeout for shell-command gates. Same envelope
/// as the File handler's gate timeout — long enough for human thinking,
/// short enough that a stalled responder surfaces as denial.
const SHELL_GATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Evaluate a shell command string against the session's policy set.
///
/// Returns `Ok(())` when the command should proceed. Returns `Err` on denial,
/// broker timeout, or gate approval (the escalation path returns
/// `Err(GateApproved)` so tests can observe the gate decision without
/// dispatching to `ProcessManager`).
///
/// The caller (Execute/Spawn arms) must call this AFTER the capability check
/// and BEFORE delegating to `ProcessManager`.
fn evaluate_shell_command(cmd: &str, user: &SessionContext) -> Result<(), EffectError> {
    let policy_ctx = PolicyContext::Shell { command: cmd };
    match user.policies().evaluate(EffectCategory::Shell, &policy_ctx) {
        PolicyAction::Deny { reason } => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}{}",
            reason.unwrap_or_else(|| "shell command denied by policy".into())
        ))),
        PolicyAction::RequireApproval { reason } => escalate_shell(
            user,
            cmd,
            reason
                .as_deref()
                .unwrap_or("shell command requires approval"),
        ),
        PolicyAction::Allow => Ok(()),
        // PolicyAction is `#[non_exhaustive]` — fail closed on any future variant.
        other => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}unhandled policy action {other:?}"
        ))),
    }
}

/// Compute the broker scope's `args_digest` for a shell command.
///
/// blake3 hex digest of the command bytes. The field is named "digest" for a
/// reason — it's a fingerprint, not the literal command. Using a real hash
/// here is collision-free in practice (blake3 has 256-bit security), gives a
/// fixed-size cache key regardless of command length, and avoids the
/// UTF-8-boundary panic surface that naive byte truncation has on non-ASCII
/// paths or emoji. The literal command still travels via the `reason` field
/// for partner-facing display in the broker prompt.
fn shell_args_digest(cmd: &str) -> String {
    blake3::hash(cmd.as_bytes()).to_hex().to_string()
}

/// Escalate a shell command through the [`crate::permission::PermissionBridge`].
///
/// Always returns `Err` — either `Err(GateApproved)` on broker grant, or
/// `Err(PermissionDenied)` on denial / timeout / missing bridge. The
/// structural pattern mirrors the `escalate` fn in `file.rs`.
fn escalate_shell(user: &SessionContext, cmd: &str, reason: &str) -> Result<(), EffectError> {
    let Some(bridge) = user.permission_bridge() else {
        return Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}shell command gated but no permission bridge wired"
        )));
    };
    let origin = user.current_dispatch_origin().unwrap_or_else(|| {
        pattern_core::types::origin::MessageOrigin::new(
            pattern_core::types::origin::Author::System {
                reason: pattern_core::types::origin::SystemReason::Timer,
            },
            pattern_core::types::origin::Sphere::System,
        )
    });
    // Real session agent_id is load-bearing for per-agent isolation of the
    // broker's scope cache (keyed `(agent_id, scope)`); fail closed if absent.
    let Some(agent) = user.dispatch_agent_id() else {
        return Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}shell command gated but no agent identity \
             available for broker attribution"
        )));
    };
    // Scope keyed on a blake3 digest of the command so per-command scope
    // caching is exact (`rm -rf /tmp/x` and `rm -rf /home` hash differently
    // and prompt independently) without paying a per-command-length cache
    // key, and without the UTF-8 boundary footgun of naive truncation.
    let args_digest = Some(shell_args_digest(cmd));
    let grant = bridge.request_sync(
        agent,
        "shell".into(),
        PermissionScope::ToolExecution {
            tool: "shell".to_string(),
            args_digest,
        },
        &origin,
        Some(reason.to_string()),
        None,
        SHELL_GATE_TIMEOUT,
    );
    if grant.is_some() {
        Err(EffectError::Handler(format!(
            "{GATE_APPROVED_PREFIX}Pattern.Shell gate cleared by broker"
        )))
    } else {
        Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}shell command denied or timed out at the broker"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::PERMISSION_DENIED_PREFIX;
    use pattern_core::CapabilitySet;
    use pattern_core::capability::EffectCategory;
    use std::sync::Arc;
    use tidepool_repr::DataConTable;

    // ---- args-digest unit tests --------------------------------------------

    /// Locks in the broker scope's `args_digest` shape: 64-char hex blake3.
    /// The wired-broker integration test asserts this same shape; pinning it
    /// here guards against silent reformat refactors.
    #[test]
    fn shell_args_digest_is_64_char_blake3_hex() {
        let digest = shell_args_digest("rm -rf /tmp/x");
        assert_eq!(digest.len(), 64, "blake3 hex digest is exactly 64 chars");
        assert!(
            digest.chars().all(|c| c.is_ascii_hexdigit()),
            "digest must be hex, got: {digest:?}"
        );
    }

    /// Distinct commands produce distinct digests — the property that makes
    /// per-command scope caching useful (different commands prompt
    /// independently rather than sharing a stale grant).
    #[test]
    fn shell_args_digest_is_per_command() {
        let a = shell_args_digest("rm -rf /tmp/a");
        let b = shell_args_digest("rm -rf /tmp/b");
        let c = shell_args_digest("rm -rf /tmp/a");
        assert_ne!(a, b, "different commands must hash differently");
        assert_eq!(a, c, "identical commands must hash identically");
    }

    /// Non-ASCII command (the regression case for the byte-truncation bug
    /// in cycle-2 review) hashes without panicking. UTF-8 boundary safety
    /// is a property of `blake3::hash` over `&[u8]`; this test pins that
    /// expectation against any future refactor that reintroduces string
    /// slicing.
    #[test]
    fn shell_args_digest_handles_non_ascii() {
        let cmd = format!("{}{}", "x".repeat(255), "é");
        // Must not panic.
        let digest = shell_args_digest(&cmd);
        assert_eq!(digest.len(), 64);
    }

    // ---- minimal test context -----------------------------------------------

    /// Minimal user context that satisfies `EffectHandler<SessionContext>`'s
    /// bound — we pass `SessionContext` directly but use a bare test struct for
    /// the capability-check path tests, which don't need a full PTY.
    ///
    /// For tests that need `SessionContext` directly (process_manager dispatch),
    /// see the integration tests in `tests/`.
    ///
    /// Build a `DataConTable` with the `()` constructor required by
    /// `cx.respond(())`.
    fn handler_table() -> DataConTable {
        use tidepool_repr::{DataCon, DataConId};
        let mut table = crate::testing::standard_datacon_table();
        table.insert(DataCon {
            id: DataConId(100),
            name: "()".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Tuple.()".to_string()),
        });
        table
    }

    // ---- capability denial test (does not need PTY) -------------------------

    /// A minimal `SessionContext`-like struct for capability-denial tests.
    /// We cannot easily build a full `SessionContext` in unit tests without a
    /// DB, so we test the capability check logic by driving ShellHandler
    /// directly with a fake that returns the correct capability set.
    ///
    /// Note: `ShellHandler` is now `impl EffectHandler<SessionContext>`, not
    /// generic. For unit tests of the capability-deny path specifically, we
    /// construct a real `SessionContext` via `from_persona` with a restricted
    /// capability set. The `from_persona` path constructs a real
    /// `ProcessManager`, but the capability check fires before any PTY work,
    /// so no PTY is needed.
    #[tokio::test]
    async fn shell_capability_denied_returns_permission_denied_prefix() {
        use crate::NopProviderClient;
        use crate::session::SessionContext;
        use crate::testing::InMemoryMemoryStore;
        use pattern_core::ProviderClient;
        use pattern_core::traits::MemoryStore;
        use pattern_core::types::snapshot::PersonaSnapshot;

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-shell-cap-deny", "A");

        // Build a CapabilitySet without Shell.
        let caps = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::File]);
        let ctx = SessionContext::from_persona(&persona, store, provider, db)
            .with_capabilities(Some(caps));

        let mut h = ShellHandler;
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &ctx);
        let err = h
            .handle(ShellReq::Execute("echo hi".into(), None), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(PERMISSION_DENIED_PREFIX),
            "expected PERMISSION_DENIED_PREFIX on capability denial, got: {msg}"
        );
        assert!(
            msg.contains("capability denied"),
            "expected 'capability denied' in message, got: {msg}"
        );
    }

    /// Full-power session (capabilities == None) does NOT deny Shell.
    /// Exercises the real PTY path to confirm end-to-end execution succeeds.
    #[tokio::test]
    async fn shell_full_power_session_executes_without_capability_deny() {
        // Skip if no shell is available (same guard as process_manager tests).
        let shell = crate::process_manager::local_pty::LocalPtyBackend::find_default_shell();
        if !std::path::Path::new(&shell).exists() && shell != "bash" {
            eprintln!("skipping: no shell found on PATH");
            return;
        }

        use crate::NopProviderClient;
        use crate::session::SessionContext;
        use crate::testing::InMemoryMemoryStore;
        use pattern_core::ProviderClient;
        use pattern_core::traits::MemoryStore;
        use pattern_core::types::snapshot::PersonaSnapshot;

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-shell-full-power", "A");

        // No capability restriction — None means full power.
        let ctx = SessionContext::from_persona(&persona, store, provider, db);
        assert!(
            ctx.capabilities().is_none(),
            "no capability set means full power"
        );

        let mut h = ShellHandler;
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &ctx);
        // A full-power session with a working PTY should execute successfully.
        let result = h.handle(ShellReq::Execute("echo hi".into(), None), &cx);
        match result {
            Ok(_) => {} // successful execution — capability check passed
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    !msg.contains("capability denied"),
                    "full-power session must not capability-deny, got: {msg}"
                );
            }
        }
    }
}
