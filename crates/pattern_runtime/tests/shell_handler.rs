// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for the Phase 3 AC3 shell-handler subsystem.
//!
//! # AC coverage
//!
//! | AC    | Test                                              | Notes                              |
//! |-------|---------------------------------------------------|------------------------------------|
//! | 3.1   | `execute_via_handler_returns_output_and_exit_code` | JSON round-trip, output + exit code |
//! | 3.2   | `execute_via_handler_persists_session_state`       | cd then pwd, same handler/context  |
//! | 3.3   | `spawn_streams_output_via_attachments`             | Drain queue; assert ShellOutput    |
//! | 3.4   | `kill_via_handler_terminates_running_process`      | Spawn, kill, drain, assert Exit    |
//! | 3.5   | `status_via_handler_lists_running_tasks`           | Two spawns; Status returns both    |
//! | 3.6   | `cwd_persists_across_handler_executions`           | pm.cwd() reflects cd change        |
//! | 3.7   | `execute_via_handler_timeout_kills_and_surfaces_error` | Timeout → Err; recovery OK      |
//! | 3.8   | `kill_unknown_task_via_handler_returns_error`      | ShellReq::Kill(bogus) → Err        |
//! | 3.9   | `exit_marker_resists_command_output_injection`     | Spurious marker; exit_code correct |
//! | 3.10  | `spawn_output_logged_to_file`                      | Log file contains OUT + EXIT lines |
//! | cap   | `execute_via_handler_denied_without_shell_capability` | Restricted caps → PERMISSION_DENIED |
//!
//! # Fixture design
//!
//! Each test builds its own `SessionContext` + `ProcessManager` with its own
//! `tempdir` for cache_dir isolation. This matches the Phase 2 file-handler
//! test approach and is safe under parallel `cargo nextest` execution.
//!
//! # Handler dispatch
//!
//! Tests call `ShellHandler::handle(req, &cx)` directly — no Haskell eval path
//! required. String responses are extracted by matching the
//! `Value::Con(text_id, [ByteArray, LitInt(0), LitInt(len)])` shape that
//! `ToCore<String>` produces, via `tidepool_bridge::FromCore::from_value`.
//!
//! # Async-reminder draining
//!
//! `spawn_output_bridge` runs on a plain `std::thread`. After a spawn, we poll
//! the async-reminder queue with a bounded timeout (up to 5 s) until the
//! expected number of attachments arrive or the Exit attachment is observed.
//! This is condition-based waiting (not arbitrary sleep), matching the
//! `writing-good-tests` skill guideline.
//!
//! # v2 semantics (Amendment 2026-04-26)
//!
//! Timeout = kill. No backgrounding. AC3.7b ("backgrounded sentinel") is
//! removed per the phase_03.md amendment. The AC3.7 test only asserts that
//! a timeout returns an error and the session recovers.

use std::sync::Arc;
use std::time::Duration;

use pattern_core::CapabilitySet;
use pattern_core::ProviderClient;
use pattern_core::capability::EffectCategory;
use pattern_core::traits::MemoryStore;
use pattern_core::types::message::{MessageAttachment, ShellOutputKind};
use pattern_core::types::snapshot::PersonaSnapshot;
use tidepool_bridge::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_repr::DataConTable;
use tidepool_testing::r#gen::standard_datacon_table;

use pattern_runtime::process_manager::ProcessManager;
use pattern_runtime::process_manager::local_pty::LocalPtyBackend;
use pattern_runtime::process_manager::types::ExecuteResult;
use pattern_runtime::sdk::handlers::shell::ShellHandler;
use pattern_runtime::sdk::requests::ShellReq;
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::{InMemoryMemoryStore, NopProviderClient, test_db};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Test guard: returns `true` if a usable shell is on PATH, `false` if not.
/// Tests that need a real PTY must call this at the top and return early if
/// it returns `false`, matching the existing guard in `local_pty.rs`.
fn shell_available() -> bool {
    let shell = LocalPtyBackend::find_default_shell();
    std::path::Path::new(&shell).exists() || shell == "bash"
}

/// Build a `DataConTable` suitable for handler dispatch tests. Includes the
/// standard constructors (I#, W#, D#, Bool, Maybe, list, Text) plus the `()`
/// constructor required by `cx.respond(())`.
fn handler_table() -> DataConTable {
    use tidepool_repr::{DataCon, DataConId};
    let mut table = standard_datacon_table();
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

/// Construct a `SessionContext` with a `ProcessManager` whose cache_dir is
/// inside `cache_dir_base`. Each test should supply its own `tempdir` so
/// tests are isolated under parallel nextest execution.
async fn make_ctx_with_cache(cache_dir: &std::path::Path) -> SessionContext {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = test_db().await;
    let persona = PersonaSnapshot::new("agent-shell-test", "A");

    // `from_persona` constructs a ProcessManager with the process's cwd and
    // `$TMPDIR/pattern` as the cache dir. We need a controlled cache_dir for
    // AC3.10 log verification, so we replace the PM after construction via the
    // builder. Use a per-test cache_dir path from the caller's tempdir.
    let pm = Arc::new(ProcessManager::new(
        std::env::temp_dir(),
        cache_dir.to_path_buf(),
    ));
    // Build context then swap in our custom PM.
    SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    )
    .with_process_manager(pm)
}

/// Construct a `SessionContext` with full capabilities (no restrictions).
async fn make_ctx(cache_dir: &std::path::Path) -> SessionContext {
    make_ctx_with_cache(cache_dir).await
}

/// Construct a `SessionContext` with a restricted `CapabilitySet` that does
/// NOT include Shell. Used for capability-denial tests.
async fn make_ctx_no_shell_cap(cache_dir: &std::path::Path) -> SessionContext {
    let caps = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::File]);
    make_ctx_with_cache(cache_dir)
        .await
        .with_capabilities(Some(caps))
}

/// Dispatch a `ShellReq` through `ShellHandler` with the given `SessionContext`.
/// Returns the handler's `Result<Value, EffectError>`.
fn dispatch(
    handler: &mut ShellHandler,
    ctx: &SessionContext,
    table: &DataConTable,
    req: ShellReq,
) -> Result<tidepool_eval::Value, EffectError> {
    let cx = EffectContext::with_user(table, ctx);
    handler.handle(req, &cx)
}

/// Extract a `String` from a handler `Value` response (a Haskell `Text` value).
/// Panics with a descriptive message if extraction fails.
fn extract_string(val: &tidepool_eval::Value, table: &DataConTable) -> String {
    <String as FromCore>::from_value(val, table).expect("expected Text string in handler response")
}

/// Parse the `Spawn` handler response. Returns `(task_id, pid)`.
///
/// Spawn responds with a JSON-encoded `{"task_id": "...", "pid": N}` (per the
/// updated `Pattern.Shell` GADT — see `Pattern/Shell.hs`). The handler's
/// `cx.respond(...)` wraps that JSON as a Haskell `Text`; we decode it to a
/// String here and parse the JSON.
fn parse_spawn_response(val: &tidepool_eval::Value, table: &DataConTable) -> (String, u32) {
    let json = extract_string(val, table);
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("spawn response must be JSON");
    let task_id = parsed["task_id"]
        .as_str()
        .expect("spawn response must have a string task_id")
        .to_string();
    let pid = parsed["pid"]
        .as_u64()
        .expect("spawn response must have an integer pid") as u32;
    (task_id, pid)
}

/// Parse the `Status` handler response. Returns the list of task_ids of
/// running tasks.
///
/// Status responds with JSON-encoded `Vec<TaskInfo>` (per the updated GADT —
/// see `Pattern.Shell.Status :: Shell Text`). Tests typically only need the
/// task_ids, so this helper extracts that subset; tests that care about the
/// `pid` / `command` / `elapsed_ms` fields can call `extract_string` directly
/// and parse the full structure.
fn parse_status_task_ids(val: &tidepool_eval::Value, table: &DataConTable) -> Vec<String> {
    let json = extract_string(val, table);
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("status response must be JSON");
    parsed
        .as_array()
        .expect("status response must be a JSON array")
        .iter()
        .map(|info| {
            info["task_id"]
                .as_str()
                .expect("each TaskInfo must have a string task_id")
                .to_string()
        })
        .collect()
}

/// Poll `ctx.async_reminder_queue()` until `predicate` returns `true` or
/// `deadline` elapses. Returns `true` if the predicate passed before timeout.
///
/// Uses condition-based waiting (not arbitrary sleep) per the
/// `writing-good-tests` skill guidance.
fn wait_for_queue<F>(ctx: &SessionContext, deadline: Duration, predicate: F) -> bool
where
    F: Fn(&[MessageAttachment]) -> bool,
{
    let end = std::time::Instant::now() + deadline;
    loop {
        {
            let q = ctx.async_reminder_queue().lock().unwrap();
            if predicate(&q) {
                return true;
            }
        }
        if std::time::Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // One last check after the deadline fires.
    let q = ctx.async_reminder_queue().lock().unwrap();
    predicate(&q)
}

/// Drain all `MessageAttachment::ShellOutput` entries from the queue for the
/// given `task_id_str`. Waits up to `deadline` for an `Exit` chunk to arrive.
/// Returns the collected attachments.
fn drain_shell_outputs(
    ctx: &SessionContext,
    task_id_str: &str,
    deadline: Duration,
) -> Vec<MessageAttachment> {
    // Wait until we see a ShellOutput::Exit for this task_id.
    wait_for_queue(ctx, deadline, |q| {
        q.iter().any(|a| match a {
            MessageAttachment::ShellOutput { task_id, kind, .. } => {
                task_id == task_id_str && matches!(kind, ShellOutputKind::Exit { .. })
            }
            _ => false,
        })
    });

    let mut q = ctx.async_reminder_queue().lock().unwrap();
    let (mine, rest): (Vec<_>, Vec<_>) = q.drain(..).partition(|a| match a {
        MessageAttachment::ShellOutput { task_id, .. } => task_id == task_id_str,
        _ => false,
    });
    // Put back unrelated attachments.
    q.extend(rest);
    mine
}

// ── AC3.1: execute returns output and exit code ───────────────────────────────

/// AC3.1 — `execute_via_handler_returns_output_and_exit_code`
///
/// Dispatches `ShellReq::Execute("echo hello", 30)` through `ShellHandler`.
/// Asserts the response JSON deserialises to `ExecuteResult` with output
/// containing "hello" and `exit_code == Some(0)`.
#[tokio::test]
async fn execute_via_handler_returns_output_and_exit_code() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let val = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("echo hello".into(), Some(30)),
    )
    .expect("execute must succeed");

    let json = extract_string(&val, &table);
    let result: ExecuteResult =
        serde_json::from_str(&json).expect("must deserialise to ExecuteResult");

    assert!(
        result.output.contains("hello"),
        "expected 'hello' in output, got: {:?}",
        result.output
    );
    assert_eq!(
        result.exit_code,
        Some(0),
        "expected exit_code == Some(0), got: {:?}",
        result.exit_code
    );
    assert!(
        result.duration_ms < 10_000,
        "duration_ms suspiciously large: {}",
        result.duration_ms
    );
}

// ── AC3.2: session state persists across executions ───────────────────────────

/// AC3.2 — `execute_via_handler_persists_session_state`
///
/// Two consecutive `ShellReq::Execute` calls through the same
/// handler/SessionContext. First `cd /tmp`, second `pwd`. The second
/// response's output must contain `/tmp`.
#[tokio::test]
async fn execute_via_handler_persists_session_state() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    // First command: change directory.
    dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("cd /tmp".into(), Some(10)),
    )
    .expect("cd must succeed");

    // Second command: verify cwd.
    let val = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("pwd".into(), Some(10)),
    )
    .expect("pwd must succeed");

    let json = extract_string(&val, &table);
    let result: ExecuteResult =
        serde_json::from_str(&json).expect("must deserialise to ExecuteResult");

    assert!(
        result.output.contains("/tmp"),
        "expected '/tmp' in pwd output, got: {:?}",
        result.output
    );
}

// ── AC3.3: spawn streams output via attachments ───────────────────────────────

/// AC3.3 — `spawn_streams_output_via_attachments`
///
/// Dispatches `ShellReq::Spawn("for i in 1 2 3; do echo line$i; sleep 0.05; done")`.
/// Drains the async-reminder queue (polling with 5s timeout). Asserts:
/// - at least one `ShellOutput { kind: Output(_), .. }` per expected line
/// - exactly one `ShellOutput { kind: Exit { code: Some(0), .. }, .. }`
/// - all attachments carry the task_id returned by the Spawn response
#[tokio::test]
async fn spawn_streams_output_via_attachments() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let val = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Spawn("for i in 1 2 3; do echo line$i; sleep 0.05; done".into()),
    )
    .expect("spawn must succeed");

    let (task_id_str, pid) = parse_spawn_response(&val, &table);
    assert!(
        !task_id_str.is_empty(),
        "spawn must return a non-empty task_id"
    );
    assert!(pid > 0, "spawn must return a non-zero pid; got: {pid}");

    // Drain: wait up to 5 s for the Exit attachment to arrive.
    let attachments = drain_shell_outputs(&ctx, &task_id_str, Duration::from_secs(5));

    // Must have at least one Output chunk per expected line.
    let combined_output: String = attachments
        .iter()
        .filter_map(|a| match a {
            MessageAttachment::ShellOutput {
                kind: ShellOutputKind::Output(text),
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    for line in &["line1", "line2", "line3"] {
        assert!(
            combined_output.contains(line),
            "expected '{line}' in combined output, got: {combined_output:?}"
        );
    }

    // Must have exactly one Exit chunk with code Some(0).
    let exits: Vec<_> = attachments
        .iter()
        .filter_map(|a| match a {
            MessageAttachment::ShellOutput {
                kind: ShellOutputKind::Exit { code, .. },
                task_id,
                ..
            } => Some((*code, task_id.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        exits.len(),
        1,
        "expected exactly one Exit attachment, got {exits:?}"
    );
    assert_eq!(
        exits[0].0,
        Some(0),
        "expected exit code Some(0), got: {:?}",
        exits[0].0
    );
    assert_eq!(
        exits[0].1, task_id_str,
        "Exit attachment task_id must match spawn response"
    );
}

// ── AC3.4: kill terminates running process ────────────────────────────────────

/// AC3.4 — `kill_via_handler_terminates_running_process`
///
/// End-to-end through the real handler: dispatches `Spawn` to get a task_id,
/// dispatches `Kill(task_id)` via the handler (no longer bypassing — the
/// post-amendment GADT takes a `TaskId :: Text` so the handler dispatch is
/// honest), and dispatches `Status` to confirm the task is no longer listed.
///
/// We hold the receiver from `pm.spawn` — the handler's bridge owns the queue
/// path; we only need the rx to drain to Exit so `Status` reflects the kill.
/// (Post-Exit the reader thread removes the entry before the Exit chunk
/// reaches the queue, per the `run_spawn_reader` ordering invariant.)
#[tokio::test]
async fn kill_via_handler_terminates_running_process() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    // Dispatch Spawn through the handler. Parse the JSON response.
    let val = dispatch(&mut h, &ctx, &table, ShellReq::Spawn("sleep 60".into()))
        .expect("spawn must succeed");
    let (task_id_str, _pid) = parse_spawn_response(&val, &table);

    // Verify the task appears in Status (via handler).
    {
        let val = dispatch(&mut h, &ctx, &table, ShellReq::Status).expect("status must succeed");
        let tasks = parse_status_task_ids(&val, &table);
        assert!(
            tasks.contains(&task_id_str),
            "expected task_id in status before kill, got: {tasks:?}"
        );
    }

    // Dispatch Kill via the handler — this is the path that was previously
    // unreachable because the GADT lied about the type.
    dispatch(&mut h, &ctx, &table, ShellReq::Kill(task_id_str.clone())).expect("kill must succeed");

    // Wait for the bridge to enqueue the Exit attachment. Once observed, the
    // task entry is removed from the running map (per the
    // `run_spawn_reader` remove-before-Exit ordering invariant).
    let attachments = drain_shell_outputs(&ctx, &task_id_str, Duration::from_secs(5));
    let saw_exit = attachments.iter().any(|a| {
        matches!(
            a,
            MessageAttachment::ShellOutput {
                kind: ShellOutputKind::Exit { .. },
                ..
            }
        )
    });
    assert!(saw_exit, "expected Exit attachment in queue after kill");

    // Status via handler must no longer list the killed task.
    let val =
        dispatch(&mut h, &ctx, &table, ShellReq::Status).expect("status after kill must succeed");
    let tasks = parse_status_task_ids(&val, &table);
    assert!(
        !tasks.contains(&task_id_str),
        "task must not appear in status after kill; got: {tasks:?}"
    );
}

// ── AC3.5: status lists running tasks ─────────────────────────────────────────

/// AC3.5 — `status_via_handler_lists_running_tasks`
///
/// Spawns two long-running commands, dispatches `ShellReq::Status`, asserts
/// the response contains both task IDs. Cleans up by killing both.
#[tokio::test]
async fn status_via_handler_lists_running_tasks() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    // Dispatch two Spawns through the handler.
    let val_a = dispatch(&mut h, &ctx, &table, ShellReq::Spawn("sleep 60".into()))
        .expect("spawn a must succeed");
    let (task_id_a, _pid_a) = parse_spawn_response(&val_a, &table);
    let val_b = dispatch(&mut h, &ctx, &table, ShellReq::Spawn("sleep 60".into()))
        .expect("spawn b must succeed");
    let (task_id_b, _pid_b) = parse_spawn_response(&val_b, &table);

    let val = dispatch(&mut h, &ctx, &table, ShellReq::Status).expect("status must succeed");
    let tasks = parse_status_task_ids(&val, &table);

    assert!(
        tasks.contains(&task_id_a),
        "status must list task A; got: {tasks:?}"
    );
    assert!(
        tasks.contains(&task_id_b),
        "status must list task B; got: {tasks:?}"
    );

    // Cleanup — Kill via handler (keeps the test on the public API surface).
    let _ = dispatch(&mut h, &ctx, &table, ShellReq::Kill(task_id_a));
    let _ = dispatch(&mut h, &ctx, &table, ShellReq::Kill(task_id_b));
}

// ── AC3.6: cwd persists across handler executions ─────────────────────────────

/// AC3.6 — `cwd_persists_across_handler_executions`
///
/// Same behavioral check as AC3.2, but additionally reads `pm.cwd()` between
/// executes to verify the backend's cwd cache is updated after each command.
#[tokio::test]
async fn cwd_persists_across_handler_executions() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    // Before any execute, cwd should be the initial cwd (process cwd or /tmp).
    let pm = ctx.process_manager().clone();
    let cwd_before = pm.cwd();
    assert!(
        cwd_before.is_some(),
        "cwd() must return Some before first execute"
    );

    // Execute cd /tmp.
    dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("cd /tmp".into(), Some(10)),
    )
    .expect("cd must succeed");

    // After cd, the backend refreshes cwd via `pwd`.
    let cwd_after = pm.cwd().expect("cwd() must be Some after execute");
    assert!(
        cwd_after.starts_with("/tmp"),
        "expected cwd to start with /tmp after 'cd /tmp', got: {cwd_after:?}"
    );

    // Execute pwd via handler and verify output agrees.
    let val = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("pwd".into(), Some(10)),
    )
    .expect("pwd must succeed");
    let json = extract_string(&val, &table);
    let result: ExecuteResult = serde_json::from_str(&json).expect("pwd result must deserialise");
    assert!(
        result.output.contains("/tmp"),
        "pwd output must contain /tmp, got: {:?}",
        result.output
    );
}

// ── AC3.7: execute timeout kills and surfaces error ───────────────────────────

/// AC3.7 — `execute_via_handler_timeout_kills_and_surfaces_error`
///
/// Dispatches `ShellReq::Execute("sleep 5", 1)`. Under the v2-semantics
/// amendment (2026-04-26), timeout = kill. The response must be
/// `Err(EffectError::Handler(_))` containing "timed out". A subsequent
/// execute through the SAME handler/context must succeed (session recovered
/// after interrupt_and_drain).
///
/// NOTE: AC3.7b (backgrounded sentinel) is removed per the amendment. We do
/// NOT test a Backgrounded ShellOutputKind here.
#[tokio::test]
async fn execute_via_handler_timeout_kills_and_surfaces_error() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let start = std::time::Instant::now();
    let err = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("sleep 5".into(), Some(1)),
    )
    .expect_err("execute with 1s timeout against 'sleep 5' must fail");

    // Must return within a reasonable bound (timeout + post-kill drain budget).
    // Drain budget grew to ~30s after the shell handler was reworked to wait
    // for the killed process's output streams to fully close before returning;
    // 60s gives enough margin under load without hiding regressions.
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(60),
        "execute should return within drain budget after timeout, elapsed: {elapsed:?}"
    );

    // Error must describe a timeout.
    let msg = err.to_string();
    assert!(
        msg.contains("timed out") || msg.contains("timeout"),
        "error must mention timeout, got: {msg}"
    );

    // Recovery: the next execute through the SAME handler/context must succeed.
    let val = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("echo recovered".into(), Some(10)),
    )
    .expect("execute after timeout must succeed");
    let json = extract_string(&val, &table);
    let result: ExecuteResult =
        serde_json::from_str(&json).expect("recovery result must deserialise");
    assert!(
        result.output.contains("recovered"),
        "expected 'recovered' in output after timeout recovery, got: {:?}",
        result.output
    );
}

// ── AC3.8: kill unknown task returns error ────────────────────────────────────

/// AC3.8 — `kill_unknown_task_via_handler_returns_error`
///
/// Dispatches `ShellReq::Kill(99999999)`. The response must be
/// `Err(EffectError::Handler(_))` containing "not found" or "unknown".
#[tokio::test]
async fn kill_unknown_task_via_handler_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let err = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Kill(99_999_999_i64.to_string()),
    )
    .expect_err("kill of unknown task must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("not found") || msg.contains("unknown") || msg.contains("Unknown"),
        "error must mention 'not found' or 'unknown', got: {msg}"
    );
}

// ── AC3.9: exit marker resists command output injection ───────────────────────

/// AC3.9 — `exit_marker_resists_command_output_injection`
///
/// Handler-level version: runs `echo '__PATTERN_EXIT_deadbeef__:1'; true`
/// via Execute. The actual exit marker is a nonce per call, so the spurious
/// string does NOT confuse the exit-code parser. Asserts `exit_code == Some(0)`.
/// (The LocalPty-level version is `exit_marker_resists_collision_in_command_output`.)
#[tokio::test]
async fn exit_marker_resists_command_output_injection() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let val = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("echo '__PATTERN_EXIT_deadbeef__:1'; true".into(), Some(10)),
    )
    .expect("execute must succeed");

    let json = extract_string(&val, &table);
    let result: ExecuteResult = serde_json::from_str(&json).expect("result must deserialise");

    // The command's true exit is 0 (`true`). The spurious marker-like string
    // in output must not hijack the exit code detection.
    assert_eq!(
        result.exit_code,
        Some(0),
        "expected exit_code Some(0) despite spurious marker in output; got: {:?}",
        result.exit_code
    );
    assert!(
        result.output.contains("__PATTERN_EXIT_deadbeef__:1"),
        "spurious marker string should appear verbatim in output, got: {:?}",
        result.output
    );
}

// ── AC3.10: spawn output logged to file ──────────────────────────────────────

/// AC3.10 — `spawn_output_logged_to_file`
///
/// Configures SessionContext with a `tempdir` as `cache_dir`. Spawns
/// `echo logged-line`. Waits for the Exit attachment to arrive in the
/// async-reminder queue. Then reads `<cache_dir>/shell/<task_id>.log` from
/// disk and asserts it contains both:
/// - a line with `OUT` and `logged-line`
/// - a line with `EXIT` and `code=Some(0)`
#[tokio::test]
async fn spawn_output_logged_to_file() {
    if !shell_available() {
        eprintln!("skipping: no shell found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let val = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Spawn("echo logged-line".into()),
    )
    .expect("spawn must succeed");

    let (task_id_str, _pid) = parse_spawn_response(&val, &table);

    // Wait for Exit attachment in the queue (confirms bridge thread finished).
    let attachments = drain_shell_outputs(&ctx, &task_id_str, Duration::from_secs(5));
    let has_exit = attachments.iter().any(|a| {
        matches!(
            a,
            MessageAttachment::ShellOutput {
                kind: ShellOutputKind::Exit { code: Some(0), .. },
                ..
            }
        )
    });
    assert!(has_exit, "must have Exit(Some(0)) attachment in queue");

    // Read the log file.
    let log_path = dir.path().join("shell").join(format!("{task_id_str}.log"));
    assert!(log_path.exists(), "log file must exist at {log_path:?}");
    let log_content = std::fs::read_to_string(&log_path).expect("log file must be readable");

    // Log must contain an OUT line with the output.
    let has_out = log_content
        .lines()
        .any(|l| l.contains("OUT") && l.contains("logged-line"));
    assert!(
        has_out,
        "log must contain 'OUT logged-line', got:\n{log_content}"
    );

    // Log must contain an EXIT line with code=Some(0).
    let has_exit_line = log_content
        .lines()
        .any(|l| l.contains("EXIT") && l.contains("code=Some(0)"));
    assert!(
        has_exit_line,
        "log must contain 'EXIT code=Some(0)', got:\n{log_content}"
    );
}

// ── capability tests ──────────────────────────────────────────────────────────

/// Capability test — `execute_via_handler_denied_without_shell_capability`
///
/// Constructs a `SessionContext` with a `CapabilitySet` that does NOT include
/// `EffectCategory::Shell`. Dispatches `ShellReq::Execute(...)`. Asserts the
/// response is `Err(EffectError::Handler(_))` containing
/// `PERMISSION_DENIED_PREFIX` and "capability denied". ProcessManager is not
/// consulted (the capability check short-circuits before any PM call).
#[tokio::test]
async fn execute_via_handler_denied_without_shell_capability() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx_no_shell_cap(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let err = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("echo hi".into(), Some(10)),
    )
    .expect_err("shell request must be denied without Shell capability");

    let msg = err.to_string();
    let prefix = pattern_runtime::policy::PERMISSION_DENIED_PREFIX;
    assert!(
        msg.contains(prefix),
        "error must contain PERMISSION_DENIED_PREFIX, got: {msg}"
    );
    assert!(
        msg.contains("capability denied"),
        "error must say 'capability denied', got: {msg}"
    );

    // Verify ProcessManager was not consulted: status() should list no tasks.
    let pm = ctx.process_manager();
    assert!(
        pm.status().is_empty(),
        "ProcessManager must not have been invoked; got tasks: {:?}",
        pm.status()
    );
}

/// Capability test — `spawn_via_handler_denied_without_shell_capability`
///
/// Same shape as the Execute version but for `ShellReq::Spawn`. Confirms the
/// capability check fires before `ProcessManager::spawn` is called.
#[tokio::test]
async fn spawn_via_handler_denied_without_shell_capability() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx_no_shell_cap(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let err = dispatch(&mut h, &ctx, &table, ShellReq::Spawn("sleep 60".into()))
        .expect_err("Spawn must be denied without Shell capability");

    let msg = err.to_string();
    let prefix = pattern_runtime::policy::PERMISSION_DENIED_PREFIX;
    assert!(
        msg.contains(prefix),
        "error must contain PERMISSION_DENIED_PREFIX, got: {msg}"
    );
    assert!(
        msg.contains("capability denied"),
        "error must say 'capability denied', got: {msg}"
    );

    // No tasks should have been spawned.
    assert!(
        ctx.process_manager().status().is_empty(),
        "ProcessManager must not have been invoked"
    );
}

/// Capability test — `kill_via_handler_denied_without_shell_capability`
///
/// Same shape for `ShellReq::Kill`. Kill is also capability-gated so an agent
/// without Shell cannot attempt to kill tasks (even tasks it theoretically
/// could not have spawned).
#[tokio::test]
async fn kill_via_handler_denied_without_shell_capability() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx_no_shell_cap(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let err = dispatch(&mut h, &ctx, &table, ShellReq::Kill("some-task-id".into()))
        .expect_err("Kill must be denied without Shell capability");

    let msg = err.to_string();
    let prefix = pattern_runtime::policy::PERMISSION_DENIED_PREFIX;
    assert!(
        msg.contains(prefix),
        "error must contain PERMISSION_DENIED_PREFIX, got: {msg}"
    );
    assert!(
        msg.contains("capability denied"),
        "error must say 'capability denied', got: {msg}"
    );
}

/// Capability test — `status_via_handler_denied_without_shell_capability`
///
/// Same shape for `ShellReq::Status`. Status is capability-gated so an agent
/// without Shell cannot enumerate running processes.
#[tokio::test]
async fn status_via_handler_denied_without_shell_capability() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx_no_shell_cap(dir.path()).await;
    let table = handler_table();
    let mut h = ShellHandler;

    let err = dispatch(&mut h, &ctx, &table, ShellReq::Status)
        .expect_err("Status must be denied without Shell capability");

    let msg = err.to_string();
    let prefix = pattern_runtime::policy::PERMISSION_DENIED_PREFIX;
    assert!(
        msg.contains(prefix),
        "error must contain PERMISSION_DENIED_PREFIX, got: {msg}"
    );
    assert!(
        msg.contains("capability denied"),
        "error must say 'capability denied', got: {msg}"
    );
}

// ── policy gate tests ─────────────────────────────────────────────────────────

/// Policy test — `execute_via_handler_denies_when_policy_denies`
///
/// Wires a `SessionContext` with a `PolicySet` containing a `Deny` rule for
/// `rm *`. Dispatches `Execute("rm /tmp/x", None)`. Asserts the response is
/// `Err(EffectError::Handler(_))` containing `PERMISSION_DENIED_PREFIX`.
/// `ProcessManager` must not be consulted (policy short-circuits before PM).
#[tokio::test]
async fn execute_via_handler_denies_when_policy_denies() {
    use pattern_core::{
        EffectCategory, PolicyAction, PolicyMatcher, PolicyRule, PolicySet, Precedence,
    };
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;

    // Inject a Deny rule for "rm *".
    let deny_rule = PolicyRule::new(
        EffectCategory::Shell,
        PolicyMatcher::ShellCommand {
            pattern: "rm *".into(),
        },
        PolicyAction::Deny {
            reason: Some("rm denied by test policy".into()),
        },
        Precedence::RuntimeOverride,
    );
    let ctx = ctx.with_policies(Arc::new(PolicySet::from_rules([deny_rule])));

    let table = handler_table();
    let mut h = ShellHandler;

    let err = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("rm /tmp/x".into(), None),
    )
    .expect_err("Execute must be denied when policy Deny fires");

    let msg = err.to_string();
    let prefix = pattern_runtime::policy::PERMISSION_DENIED_PREFIX;
    assert!(
        msg.contains(prefix),
        "error must contain PERMISSION_DENIED_PREFIX, got: {msg}"
    );

    // ProcessManager must not have been invoked — no tasks should exist.
    assert!(
        ctx.process_manager().status().is_empty(),
        "ProcessManager must not have been invoked after policy Deny"
    );
}

/// Policy test — `execute_via_handler_fails_closed_when_require_approval_without_bridge`
///
/// Wires a `SessionContext` with `rust_defaults()` policies (which includes
/// `rm -rf*` as `RequireApproval`). No permission bridge is wired. Dispatches
/// `Execute("rm -rf /tmp/x", None)`. The handler must attempt broker escalation;
/// without a bridge, it fails closed with `PERMISSION_DENIED_PREFIX`.
///
/// This verifies the fail-closed path. The wired-broker variant
/// (`execute_via_handler_escalates_to_broker_with_observed_scope`) verifies
/// the broker actually receives the expected `ToolExecution` scope.
#[tokio::test]
async fn execute_via_handler_fails_closed_when_require_approval_without_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;
    // rust_defaults() includes RequireApproval for "rm -rf*"; no bridge wired.

    let table = handler_table();
    let mut h = ShellHandler;

    let err = dispatch(
        &mut h,
        &ctx,
        &table,
        ShellReq::Execute("rm -rf /tmp/x".into(), None),
    )
    .expect_err("Execute must be denied when no bridge wired for RequireApproval");

    let msg = err.to_string();
    let prefix = pattern_runtime::policy::PERMISSION_DENIED_PREFIX;
    assert!(
        msg.contains(prefix),
        "error must contain PERMISSION_DENIED_PREFIX on bridge-absent escalation, got: {msg}"
    );

    // The error must NOT be a capability-denied error — it must be the
    // policy/broker path (capability check passed; rm -rf hit the gate).
    assert!(
        !msg.contains("capability denied"),
        "error must be policy/broker denial, not capability denial, got: {msg}"
    );
}

/// Policy test — `spawn_via_handler_also_gates_on_policy`
///
/// Same as the Deny test for Execute but for `ShellReq::Spawn`. Confirms the
/// policy gate fires on Spawn as well as Execute.
#[tokio::test]
async fn spawn_via_handler_also_gates_on_policy() {
    use pattern_core::{
        EffectCategory, PolicyAction, PolicyMatcher, PolicyRule, PolicySet, Precedence,
    };
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let ctx = make_ctx(dir.path()).await;

    let deny_rule = PolicyRule::new(
        EffectCategory::Shell,
        PolicyMatcher::ShellCommand {
            pattern: "rm *".into(),
        },
        PolicyAction::Deny {
            reason: Some("rm denied by test policy".into()),
        },
        Precedence::RuntimeOverride,
    );
    let ctx = ctx.with_policies(Arc::new(PolicySet::from_rules([deny_rule])));

    let table = handler_table();
    let mut h = ShellHandler;

    let err = dispatch(&mut h, &ctx, &table, ShellReq::Spawn("rm /tmp/x".into()))
        .expect_err("Spawn must be denied when policy Deny fires");

    let msg = err.to_string();
    let prefix = pattern_runtime::policy::PERMISSION_DENIED_PREFIX;
    assert!(
        msg.contains(prefix),
        "error must contain PERMISSION_DENIED_PREFIX on Spawn with Deny rule, got: {msg}"
    );

    // No tasks should be running — the gate must have fired before PM.spawn.
    assert!(
        ctx.process_manager().status().is_empty(),
        "ProcessManager::spawn must not have been called when policy Deny fires"
    );
}

/// Policy test — `execute_via_handler_escalates_to_broker_with_observed_scope`
///
/// Wires a real `PermissionBroker` + `PermissionBridge` into a SessionContext,
/// subscribes to the broker, dispatches `Execute("rm -rf /tmp/x", None)` (which
/// hits `rust_defaults()`'s `RequireApproval` rule), and asserts:
///
/// 1. The broker observes a request on the subscription channel.
/// 2. The observed scope is `PermissionScope::ToolExecution { tool: "shell",
///    args_digest: Some(_) }` — i.e., the handler's `escalate_shell` actually
///    constructs the expected scope shape.
/// 3. The args_digest is non-empty (a blake3 hex digest of the command).
/// 4. After the responder Denies, the handler returns
///    `PERMISSION_DENIED_PREFIX`.
///
/// Mirrors `pattern_runtime::sdk::handlers::file::tests::config_kdl_write_escalates_to_broker_and_can_be_denied`
/// — same structural pattern, different scope shape. Locks in the per-command
/// scope-caching contract: a refactor that, e.g., zeroed `args_digest` to
/// `None` would silently weaken the grant boundary; this test catches it.
#[tokio::test]
async fn execute_via_handler_escalates_to_broker_with_observed_scope() {
    use pattern_core::permission::{PermissionBroker, PermissionDecisionKind, PermissionScope};
    use pattern_runtime::permission::PermissionBridge;

    let dir = tempfile::tempdir().unwrap();
    let ctx_base = make_ctx(dir.path()).await;

    // Real broker + bridge. Subscribe before spawning the responder so we
    // cannot miss the request.
    let broker = Arc::new(PermissionBroker::new());
    let mut rx = broker.subscribe();
    let observed_scope = Arc::new(std::sync::Mutex::new(None));
    let observed_for_thread = observed_scope.clone();
    let broker_for_responder = broker.clone();
    let responder = tokio::spawn(async move {
        if let Ok(req) = rx.recv().await {
            *observed_for_thread.lock().unwrap() = Some(req.scope.clone());
            broker_for_responder
                .resolve(&req.id, PermissionDecisionKind::Deny)
                .await;
        }
    });

    let bridge = Arc::new(PermissionBridge::spawn(broker));
    let ctx = ctx_base.with_permission_bridge(bridge);

    let bridge_command = "rm -rf /tmp/x";
    let table = handler_table();
    let mut h = ShellHandler;

    // Dispatch in a blocking task — the handler is sync but consults the
    // bridge which talks to the async broker.
    let result = tokio::task::spawn_blocking(move || {
        let cx = EffectContext::with_user(&table, &ctx);
        h.handle(ShellReq::Execute(bridge_command.into(), None), &cx)
    })
    .await
    .expect("blocking task")
    .expect_err("denial should surface");

    let msg = result.to_string();
    assert!(
        msg.contains(pattern_runtime::policy::PERMISSION_DENIED_PREFIX),
        "expected PERMISSION_DENIED_PREFIX after broker denial, got: {msg}"
    );
    responder.await.unwrap();

    // The broker must have seen exactly the scope shape the handler claims to
    // construct: ToolExecution { tool: "shell", args_digest: Some(<digest>) }.
    let scope = observed_scope.lock().unwrap().clone();
    match scope {
        Some(PermissionScope::ToolExecution { tool, args_digest }) => {
            assert_eq!(tool, "shell", "tool must be 'shell', got: {tool}");
            let digest = args_digest.expect(
                "args_digest must be Some(_) — null digest defeats per-command grant caching",
            );
            // blake3 hex digests are 64 chars. We don't pin the exact value —
            // this test asserts the shape and non-emptiness; the handler's
            // unit test (`shell_args_digest_is_blake3_hex`) pins format.
            assert_eq!(
                digest.len(),
                64,
                "args_digest must be a 64-char blake3 hex, got {} chars: {digest:?}",
                digest.len()
            );
            assert!(
                digest.chars().all(|c| c.is_ascii_hexdigit()),
                "args_digest must be hex, got: {digest:?}"
            );
        }
        other => panic!(
            "expected PermissionScope::ToolExecution {{ tool: \"shell\", args_digest: Some(_) }}, got: {other:?}"
        ),
    }
}
