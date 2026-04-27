//! End-to-end sandbox-IO smoke test — Phase 5 AC5.3 / AC5.6 / AC5.7.
//!
//! Drives a real [`TidepoolSession`] opened via
//! [`TidepoolSession::open_with_agent_loop`] against scripted Haskell
//! agent programs delivered through [`MockProviderClient::tool_use_turn`]
//! (`code` tool calls). The session's [`EvalWorker`] compiles and runs
//! each program against the full SDK bundle; effects flow through the
//! production handler-dispatch path (Shell ↔ ProcessManager, File ↔
//! FileManager, Port ↔ PortRegistry → HttpPort / MockPort).
//!
//! Why this shape: it short-circuits running `pattern-server` + the TUI
//! by hand. If this test passes, the end-to-end wire-turn loop, the
//! GADT decode/encode boundary, the FileManager/ProcessManager/PortRegistry
//! wiring, the async-reminder splice path, and capability/policy gating
//! all work against the daemon's canonical session-open path.
//!
//! # Multi-thread runtime required
//!
//! `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]` —
//! `PortHandler::handle` calls `tokio::sync::mpsc::Sender::blocking_send`
//! to the dispatcher actor, which deadlocks on a single-thread runtime.
//!
//! # Determinism
//!
//! - All filesystem state lives in tempdirs (AC5.7 — no shared `/tmp`
//!   paths).
//! - The HTTP probe URL is supplied by `wiremock::MockServer`.
//! - The `mock` port's call response is set before the agent dispatches
//!   `Port.call "mock"`.
//! - Reminder-observation steps (4, 6, 9) wait condition-based for the
//!   queue to reach the expected attachment kind before driving
//!   `step_with_agent_loop`.
//!
//! # Labeled assertions
//!
//! Every assertion identifies the step ("step N: …") so AC5.6 ("error
//! identifies which step and which assertion") is satisfied.

#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;
use std::time::Duration;

use pattern_core::CapabilitySet;
use pattern_core::ProviderClient;
use pattern_core::capability::EffectCategory;
use pattern_core::traits::{MemoryStore, PortRegistry, TurnSink, VecSink};
use pattern_core::types::ids::{BatchId, MessageId, new_id, new_snowflake_id};
use pattern_core::types::message::{FileEditKind, Message, MessageAttachment, ShellOutputKind};
use pattern_core::types::origin::{Author, MessageOrigin, Partner, Sphere};
use pattern_core::types::port::PortId;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::types::turn::TurnInput;
use smol_str::SmolStr;

use pattern_runtime::SdkLocation;
use pattern_runtime::file_manager::{FilePolicy, RuleMode};
use pattern_runtime::port_registry::PortRegistryImpl;
use pattern_runtime::ports::http::HttpPort;
use pattern_runtime::session::{SessionRegistries, TidepoolSession};
use pattern_runtime::testing::{InMemoryMemoryStore, MockPort, MockProviderClient, test_db};

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Poll `predicate` every 10ms until it returns true or `deadline` expires.
fn wait_condition(deadline: Duration, predicate: impl Fn() -> bool) -> bool {
    let end = std::time::Instant::now() + deadline;
    loop {
        if predicate() {
            return true;
        }
        if std::time::Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    predicate()
}

/// Tokio-aware variant: yields between polls so the dispatcher / drain
/// tasks can make progress.
async fn wait_condition_async(deadline: Duration, predicate: impl Fn() -> bool) -> bool {
    let end = std::time::Instant::now() + deadline;
    loop {
        if predicate() {
            return true;
        }
        if std::time::Instant::now() >= end {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    predicate()
}

/// Insert the agent row required by `messages.agent_id`'s FK.
async fn ensure_agent_row(db: &pattern_db::ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: agent_id.to_string(),
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
    let _ = pattern_db::queries::create_agent(&db.get().unwrap(), &agent);
}

/// Build a `FilePolicy` allow-listing `dir` and its descendants.
fn allow_dir_policy(dir: &std::path::Path) -> FilePolicy {
    let dir_str = dir.display().to_string();
    let subtree = format!("{dir_str}/**");
    FilePolicy::from_rules(vec![(RuleMode::Allow, dir_str), (RuleMode::Allow, subtree)])
        .expect("step 0: build allow policy")
}

/// Each turn input carries a single user message so reminder splicing has
/// a concrete attachment target. (`drive_step` synthesises a blank user
/// message when the input is empty, but using a real one keeps the
/// assertions easier to follow.)
fn user_input(agent_id: &str, text: &str) -> TurnInput {
    let batch = BatchId::from(new_snowflake_id());
    let msg = Message {
        chat_message: genai::chat::ChatMessage::user(text),
        id: MessageId::from(new_id()),
        position: new_snowflake_id(),
        owner_id: pattern_core::types::ids::AgentId::from(agent_id),
        created_at: jiff::Timestamp::now(),
        batch: batch.clone(),
        response_meta: None,
        block_refs: vec![],
        attachments: vec![],
    };
    TurnInput {
        turn_id: new_snowflake_id(),
        batch_id: batch,
        origin: MessageOrigin::new(
            Author::Partner(Partner {
                user_id: pattern_core::types::ids::new_id(),
                display_name: None,
            }),
            Sphere::Private,
        ),
        messages: vec![msg],
    }
}

/// One scripted exchange: tool_use(`code` body) → text final-turn.
/// Two wire turns. The eval worker compiles + runs the code; the second
/// turn ends the wire-turn loop with `stop_reason = EndTurn`.
///
/// `imports` is forwarded as the `code` tool's `imports` field — used
/// for non-SDK modules like `Pattern.Http` (delivered via
/// `Port::library()`, not auto-imported by the preamble).
fn agent_exchange(
    call_id: &str,
    haskell: &str,
    imports: Option<&str>,
) -> Vec<Vec<genai::chat::ChatStreamEvent>> {
    let mut args = json!({ "code": haskell });
    if let Some(imp) = imports {
        args["imports"] = serde_json::Value::String(imp.to_string());
    }
    vec![
        MockProviderClient::tool_use_turn(call_id, "code", args),
        MockProviderClient::text_turn("ok"),
    ]
}

// ── the smoke test ──────────────────────────────────────────────────────────

/// Print a short banner to stdout so `cargo nextest run --no-capture`
/// (or running the test binary directly) shows progress as each step
/// executes. Captured by default; `--no-capture` reveals.
fn banner(step: &str, summary: &str) {
    println!("[smoke] {step}: {summary}");
}

/// Print a labelled snippet on a single line.
///
/// `body` is typically a tool_result blob produced by Pattern's
/// pipeline: the agent returned a Haskell `Text`, which
/// `paginateResult 4096 (toJSON _r)` wraps as a JSON string, which the
/// tool_result envelope wraps again. The result is multiply-escaped
/// JSON that's basically unreadable in raw form.
///
/// To produce readable output:
///
/// 1. Peel JSON-string layers: repeatedly parse the body as a JSON
///    string and recurse on its decoded content. Stop at the first
///    layer that isn't a JSON string.
/// 2. Escape only the embedded newlines (so the output stays on one
///    line — important for piping through `grep`).
/// 3. Cap at ~400 chars so giant payloads don't blow up the log.
fn snippet(label: &str, body: &str) {
    let unwrapped = unwrap_json_layers(body);
    let cap = 400usize;
    let trimmed = if unwrapped.chars().count() > cap {
        let s: String = unwrapped.chars().take(cap).collect();
        format!("{}…<+{} more chars>", s, unwrapped.chars().count() - cap)
    } else {
        unwrapped
    };
    let one_line = trimmed.replace('\n', "\\n");
    println!("[smoke]   {label}: {one_line}");
}

/// Repeatedly parse `body` as a JSON string and follow the decoded
/// content until parsing fails or we hit a non-string Value. Returns
/// the deepest unwrapped string. Bounded to 6 iterations as a
/// safety net.
fn unwrap_json_layers(body: &str) -> String {
    let mut current = body.trim().to_string();
    for _ in 0..6 {
        match serde_json::from_str::<serde_json::Value>(&current) {
            Ok(serde_json::Value::String(s)) => {
                current = s;
                continue;
            }
            // Any non-string Value (object, array, number, etc.):
            // pretty-print so nested fields are visible without escaping.
            Ok(other) => {
                return serde_json::to_string(&other).unwrap_or_else(|_| other.to_string());
            }
            // Not parseable as JSON: return as-is.
            Err(_) => return current,
        }
    }
    current
}

/// Exercises shell / file / port / capability / policy through the real
/// Tidepool eval pipeline. Skipped silently if `tidepool-extract` is not
/// available — the failure-mode tests at `tests/error_clarity.rs` cover
/// the missing-binary case.
///
/// Stdout is captured by default. Run with `cargo nextest run -p
/// pattern-runtime --test sandbox_io_smoke --no-capture` (or `cargo test
/// -p pattern-runtime --test sandbox_io_smoke -- --nocapture`) to see
/// the per-step banner output.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sandbox_io_smoke_end_to_end() {
    if pattern_runtime::preflight::check().is_err() {
        eprintln!("sandbox_io_smoke: skipping — tidepool-extract not available");
        return;
    }

    // ── step 0: setup ──────────────────────────────────────────────────────
    let project_dir = tempfile::tempdir().expect("step 0: project tempdir");
    let deny_dir = tempfile::tempdir().expect("step 0: deny dir");
    banner(
        "step 0",
        &format!(
            "project_dir={} deny_dir={}",
            project_dir.path().display(),
            deny_dir.path().display(),
        ),
    );

    banner("step 0", "starting wiremock server for HttpPort");
    // Wiremock server for HttpPort.
    let mock_http = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/probe"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"http_marker":"http-roundtrip-ok"}"#),
        )
        .mount(&mock_http)
        .await;
    let http_url = format!("{}/probe", mock_http.uri());

    // Port registry: register HttpPort (real, used for the http call) and a
    // live MockPort (used for both Port.call and Port.subscribe).
    let registry = Arc::new(PortRegistryImpl::new(&tokio::runtime::Handle::current()));
    registry
        .register_sync(Arc::new(HttpPort::new()) as Arc<dyn pattern_core::traits::Port>)
        .expect("step 0: register HttpPort");
    let mock_port = MockPort::new_live("mock");
    let mock_port_for_push = Arc::clone(&mock_port);
    registry
        .register_sync(mock_port as Arc<dyn pattern_core::traits::Port>)
        .expect("step 0: register MockPort");
    assert!(
        registry.get(&PortId::new("http")).is_some(),
        "step 0: registry must hold http port after register"
    );
    assert!(
        registry.get(&PortId::new("mock")).is_some(),
        "step 0: registry must hold mock port after register"
    );

    // Pre-set the mock port's Call response.
    {
        let p = registry
            .get(&PortId::new("mock"))
            .expect("step 0: mock port lookup");
        let mp = p
            .as_any()
            .downcast_ref::<MockPort>()
            .expect("step 0: mock port downcast");
        mp.set_call_response(Ok(json!({"mock_marker":"mock-roundtrip-ok"})));
    }

    // FilePolicy allowing the project tempdir.
    let file_policy = allow_dir_policy(project_dir.path());

    // Persona with full capabilities. The `mock` and `http` ports must be
    // in the per-port allowlist; `CapabilitySet::all()` already emits a
    // wildcard for resources, so explicit per-port resources are not
    // strictly required — but listing them keeps intent visible.
    let agent_id = "agent-smoke";
    // Use `CapabilitySet::all()` so the agent's `type M` row includes
    // every effect the bundle defines (Wake + Fronting + Port). Filtering
    // any of them out shifts JIT effect tag positions: e.g. dropping
    // Wake+Fronting from the agent's row makes the agent's Port handler
    // tag 14 (vs 16 in the bundle), which dispatches Port.Call to
    // WakeHandler instead. Per-port gating via `with_resources` is the
    // right scope-restriction knob; the `all()` baseline keeps tag
    // positions aligned. (See the merge note in
    // `pattern_runtime::sdk::preamble::build_for` for the contract.)
    let caps = CapabilitySet::all().with_resources(
        EffectCategory::Port,
        [SmolStr::from("mock"), SmolStr::from("http")],
    );

    // File path the agent will write + later be modified externally.
    let smoke_file = project_dir.path().join("smoke.txt");
    let smoke_path = smoke_file.display().to_string();

    // Scripted provider — sequence of exchanges the agent will run.
    //
    // Step 1: shell + file + mock-port + http-port in one program.
    // Step 2: observe FileEdit attachment (agent re-reads file).
    // Step 3: shell.spawn streaming.
    // Step 4: observe ShellOutput attachments (agent does no-op).
    // Step 5: subscribe to mock port.
    // Step 6: observe PortEvent attachment (agent does no-op).
    // The `code` tool's `template_source` prefixes each line of the
    // supplied snippet with exactly four spaces — meaning the lines
    // themselves must be flush-left, otherwise the templated output
    // ends up with mismatched do-block indentation and GHC throws a
    // layout error. All snippets below stay flush-left.
    //
    // The HTTP call uses the typed `Pattern.Http.httpGet` wrapper
    // (delivered via `HttpPort::library()` and materialized into the
    // session's port-lib tempdir at session open). If port-library
    // delivery breaks, agent compilation fails on the qualified
    // `Http.httpGet` reference — this is the regression guard that
    // keeps the plugin-style delivery honest.
    let code_step1 = format!(
        "_ <- Log.info \"step 1 start\"\n\
         shellOut <- Shell.execute \"echo shell-roundtrip-marker\"\n\
         _initial <- File.open \"{path}\"\n\
         File.write \"{path}\" \"agent-file-marker\"\n\
         fileOut <- File.read \"{path}\"\n\
         mockOut <- Port.call \"mock\" \"ping\" \"{{}}\"\n\
         httpOut <- Http.httpGet \"{url}\"\n\
         pure (T.concat [shellOut, \"|\", fileOut, \"|\", mockOut, \"|\", httpOut])",
        path = smoke_path,
        url = http_url
    );
    let code_step2 = format!(
        "_ <- Log.info \"step 2 start\"\n\
         reread <- File.read \"{path}\"\n\
         pure reread",
        path = smoke_path,
    );
    let code_step3 = "_ <- Log.info \"step 3 start\"\n\
         taskJson <- Shell.spawn \"for i in 1 2 3; do echo line$i; sleep 0.05; done\"\n\
         pure taskJson"
        .to_string();
    let code_step4 = "_ <- Log.info \"step 4 noop (observe ShellOutput)\"\n\
         pure (\"step4-noop\" :: T.Text)"
        .to_string();
    let code_step5 = "_ <- Log.info \"step 5 subscribe\"\n\
         Port.subscribe \"mock\" \"{}\"\n\
         pure (\"subscribed\" :: T.Text)"
        .to_string();
    let code_step6 = "_ <- Log.info \"step 6 noop (observe PortEvent)\"\n\
         pure (\"step6-noop\" :: T.Text)"
        .to_string();

    let mut scripts: Vec<Vec<genai::chat::ChatStreamEvent>> = Vec::new();
    // Step 1 imports Pattern.Http (port-delivered, not in the SDK row)
    // so the typed `Http.httpGet` wrapper is in scope.
    scripts.extend(agent_exchange(
        "toolu_01_main",
        &code_step1,
        Some("import qualified Pattern.Http as Http"),
    ));
    scripts.extend(agent_exchange("toolu_02_observe_file", &code_step2, None));
    scripts.extend(agent_exchange("toolu_03_spawn", &code_step3, None));
    scripts.extend(agent_exchange("toolu_04_observe_shell", &code_step4, None));
    scripts.extend(agent_exchange("toolu_05_subscribe", &code_step5, None));
    scripts.extend(agent_exchange("toolu_06_observe_port", &code_step6, None));

    let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(scripts));

    // Build the rest of the session inputs.
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let db = test_db().await;
    ensure_agent_row(&db, agent_id).await;
    let persona = PersonaSnapshot::new(agent_id, "Smoke");
    let sdk = SdkLocation::default();
    let sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());

    // Bootstrap an initial file so File.read in step 1 always sees something
    // (even though step 1 writes first, having the file present means the
    // DirWatcher path used by File.open/Watch in later steps is exercised
    // against an existing file).
    std::fs::write(&smoke_file, "initial-disk-content")
        .expect("step 0: bootstrap smoke file write");

    let session = TidepoolSession::open_with_agent_loop(
        persona,
        &sdk,
        store,
        provider,
        db.clone(),
        tokio::runtime::Handle::current(),
        sink,
        None, // prelude_dir — SDK bundles its own.
        None, // mount_path — no project mount in this test.
        Some(caps),
        Some(SessionRegistries {
            agent_registry: None,
            router_registry: None,
            wake_registry_extras: None,
            fronting_set: None,
            port_registry: Some(Arc::clone(&registry)),
            file_policy: Some(file_policy),
        }),
    )
    .await
    .expect("step 0: open_with_agent_loop must succeed end-to-end");

    // ── step 1: shell + file + ports through Haskell ──────────────────────
    banner(
        "step 1",
        "driving shell + file + mock-port + http-port (via Pattern.Http) in one Haskell program",
    );
    let reply1 = session
        .step_with_agent_loop(user_input(agent_id, "do the work"))
        .await
        .expect("step 1: step_with_agent_loop must succeed");

    assert_eq!(
        reply1.turns.len(),
        2,
        "step 1: expected 2 wire turns (tool_use + final text), got {}",
        reply1.turns.len()
    );

    // Pull the tool_result text out of TurnHistory and assert all four
    // markers round-tripped through the production handler dispatch path.
    let combined1 = collect_tool_result_strings(&session);
    let combined1_str = combined1.join("\n");
    snippet("step 1 tool_result", &combined1_str);
    for marker in &[
        "shell-roundtrip-marker",
        "agent-file-marker",
        "mock-roundtrip-ok",
        "http-roundtrip-ok",
    ] {
        assert!(
            combined1_str.contains(marker),
            "step 1: expected marker {marker:?} in tool_result content; got: {combined1_str}"
        );
    }
    banner(
        "step 1",
        "OK — all four round-trip markers present in tool_result",
    );

    // Disk side-effect from File.write must have landed.
    let disk1 = std::fs::read_to_string(&smoke_file).expect("step 1: re-read smoke file from disk");
    snippet("step 1 disk content", &disk1);
    assert!(
        disk1.contains("agent-file-marker"),
        "step 1: smoke file disk content must contain agent-file-marker; got: {disk1:?}"
    );

    // ── step 2: external edit + FileEdit attachment ───────────────────────
    banner(
        "step 2",
        "writing externally-modified-content via std::fs::write (simulating an out-of-band editor)",
    );
    // Brief settle so the watcher's debounce window doesn't conflate the
    // step-1 write with the external edit.
    std::thread::sleep(Duration::from_millis(150));
    std::fs::write(&smoke_file, "externally-modified-content").expect("step 2: external write");

    // Wait for the DirWatcher to enqueue a FileEdit attachment.
    let got_edit = wait_condition_async(Duration::from_secs(5), || {
        session
            .context()
            .async_reminder_queue()
            .lock()
            .unwrap()
            .iter()
            .any(|a| matches!(a, MessageAttachment::FileEdit { .. }))
    })
    .await;
    assert!(
        got_edit,
        "step 2: DirWatcher must enqueue a FileEdit attachment within 5s"
    );

    let reply2 = session
        .step_with_agent_loop(user_input(agent_id, "what changed?"))
        .await
        .expect("step 2: step_with_agent_loop must succeed");
    assert_eq!(
        reply2.turns.len(),
        2,
        "step 2: expected 2 wire turns; got {}",
        reply2.turns.len()
    );

    // The FileEdit attachment must have been spliced onto step 2's first
    // input message in TurnHistory.
    let file_edit = find_attachment(&session, |a| {
        matches!(a, MessageAttachment::FileEdit { .. })
    })
    .expect("step 2: FileEdit attachment must appear in TurnHistory");
    match &file_edit {
        MessageAttachment::FileEdit { path, kind, .. } => {
            println!(
                "[smoke]   FileEdit attachment: path={} kind={:?}",
                path.display(),
                kind
            );
            assert!(
                path.to_string_lossy().contains("smoke.txt"),
                "step 2: FileEdit path must reference smoke.txt; got: {path:?}"
            );
            assert!(
                matches!(kind, FileEditKind::Open),
                "step 2: FileEdit kind must be Open (file was open at edit time); got: {kind:?}"
            );
        }
        _ => unreachable!(),
    }

    // The agent re-read the file and the post-external content should be
    // visible in its tool_result for step 2.
    let combined2 = collect_tool_result_strings(&session);
    snippet(
        "step 2 tool_result",
        combined2.last().map(String::as_str).unwrap_or(""),
    );
    assert!(
        combined2
            .iter()
            .any(|s| s.contains("externally-modified-content")),
        "step 2: agent re-read should observe external edit; got: {combined2:?}"
    );
    banner(
        "step 2",
        "OK — FileEdit reminder spliced AND agent observed external content",
    );

    // ── step 3: shell.spawn streaming ─────────────────────────────────────
    banner(
        "step 3",
        "agent calls Shell.spawn for a 3-line streaming command",
    );
    let reply3 = session
        .step_with_agent_loop(user_input(agent_id, "spawn"))
        .await
        .expect("step 3: step_with_agent_loop must succeed");
    assert_eq!(reply3.turns.len(), 2, "step 3: expected 2 wire turns");

    // The agent's tool_result is the JSON body produced by Shell.spawn:
    // `{"task_id":"...","pid":N}`. After `unwrap_json_layers` peels the
    // tool_result envelope's transport-side stringification, the
    // remaining text parses cleanly as a JSON object, so we lift the
    // task_id directly without any byte-level scanning.
    let spawn_results = collect_tool_result_strings(&session);
    let task_id = spawn_results
        .iter()
        .find_map(|s| {
            let unwrapped = unwrap_json_layers(s);
            serde_json::from_str::<serde_json::Value>(&unwrapped)
                .ok()?
                .get("task_id")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            panic!(
                "step 3: spawn task_id must appear in tool_result content; got {} entries:\n---\n{}\n---",
                spawn_results.len(),
                spawn_results.join("\n=== entry ===\n"),
            )
        });
    assert!(
        !task_id.is_empty(),
        "step 3: extracted spawn task_id must be non-empty"
    );
    banner("step 3", &format!("spawn returned task_id={task_id}"));

    // Wait for the spawned process to finish and emit Exit on the queue.
    let got_exit = wait_condition(Duration::from_secs(10), || {
        session
            .context()
            .async_reminder_queue()
            .lock()
            .unwrap()
            .iter()
            .any(|a| {
                matches!(
                    a,
                    MessageAttachment::ShellOutput { task_id: t, kind, .. }
                    if t == &task_id && matches!(kind, ShellOutputKind::Exit { .. })
                )
            })
    });
    assert!(
        got_exit,
        "step 3: ShellOutput Exit for task {task_id} must arrive within 10s"
    );
    banner(
        "step 3",
        "OK — Exit ShellOutput observed in async-reminder queue",
    );

    // ── step 4: observe ShellOutput attachments ───────────────────────────
    banner(
        "step 4",
        "agent no-op turn; expecting ShellOutput attachments to splice onto its input",
    );
    let reply4 = session
        .step_with_agent_loop(user_input(agent_id, "what came out?"))
        .await
        .expect("step 4: step_with_agent_loop must succeed");
    assert_eq!(reply4.turns.len(), 2, "step 4: expected 2 wire turns");

    let attachments4 = collect_attachments(&session);
    let shell_output_count = attachments4
        .iter()
        .filter(|a| matches!(a, MessageAttachment::ShellOutput { .. }))
        .count();
    println!("[smoke]   total ShellOutput attachments in history: {shell_output_count}");
    assert!(
        shell_output_count >= 1,
        "step 4: at least one ShellOutput attachment must be present in history; got {shell_output_count}"
    );

    // Print every ShellOutput attachment for this task_id so the operator
    // can see exactly what the shell produced — both Output chunks and
    // the terminal Exit. This is the cross-check that the agent's
    // captured stream matches what the spawned command actually printed.
    for a in &attachments4 {
        if let MessageAttachment::ShellOutput {
            task_id: t,
            kind,
            at,
            ..
        } = a
            && t == &task_id
        {
            match kind {
                ShellOutputKind::Output(text) => {
                    snippet(&format!("ShellOutput @ {at} task={t}"), text.trim_end());
                }
                ShellOutputKind::Exit { code, duration_ms } => {
                    println!(
                        "[smoke]   ShellOutput Exit @ {at} task={t} code={code:?} duration_ms={duration_ms}"
                    );
                }
                ShellOutputKind::Backgrounded { partial_output } => {
                    snippet(
                        &format!("ShellOutput Backgrounded @ {at} task={t}"),
                        partial_output.trim_end(),
                    );
                }
            }
        }
    }

    // Output text should contain at least one of the lines emitted by the
    // spawned loop, and there should be exactly one Exit.
    let outputs: Vec<&str> = attachments4
        .iter()
        .filter_map(|a| match a {
            MessageAttachment::ShellOutput {
                task_id: t,
                kind: ShellOutputKind::Output(text),
                ..
            } if t == &task_id => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let combined_output: String = outputs.join("");
    let saw_a_line = ["line1", "line2", "line3"]
        .iter()
        .any(|m| combined_output.contains(m));
    assert!(
        saw_a_line,
        "step 4: combined ShellOutput text should contain at least one of \
         line1/line2/line3; got: {combined_output:?}"
    );
    let exits: Vec<_> = attachments4
        .iter()
        .filter(|a| {
            matches!(
                a,
                MessageAttachment::ShellOutput {
                    task_id: t,
                    kind: ShellOutputKind::Exit { .. },
                    ..
                } if t == &task_id
            )
        })
        .collect();
    assert_eq!(
        exits.len(),
        1,
        "step 4: expected exactly one Exit ShellOutput for task {task_id}; got {}",
        exits.len()
    );
    banner(
        "step 4",
        &format!(
            "OK — agent received {} Output chunks + 1 Exit covering line1/line2/line3",
            outputs.len()
        ),
    );

    // ── step 5: port subscribe ────────────────────────────────────────────
    banner("step 5", "agent calls Port.subscribe \"mock\"");
    let reply5 = session
        .step_with_agent_loop(user_input(agent_id, "subscribe please"))
        .await
        .expect("step 5: step_with_agent_loop must succeed");
    assert_eq!(reply5.turns.len(), 2, "step 5: expected 2 wire turns");

    // Push a port event AFTER subscribe completed.
    let pushed_payload = json!({"event":"smoke-test","port_marker":"port-roundtrip-ok"});
    println!("[smoke]   pushing PortEvent into MockPort: {pushed_payload}");
    mock_port_for_push.push_event_live(pattern_core::types::port::PortEvent::new(
        PortId::new("mock"),
        pushed_payload.clone(),
        jiff::Timestamp::now(),
    ));

    // Wait for the drain task to deliver the PortEvent.
    let got_port_event = wait_condition_async(Duration::from_secs(5), || {
        session
            .context()
            .async_reminder_queue()
            .lock()
            .unwrap()
            .iter()
            .any(|a| matches!(a, MessageAttachment::PortEvent { .. }))
    })
    .await;
    assert!(
        got_port_event,
        "step 5: drain task must enqueue a PortEvent attachment within 5s"
    );
    banner(
        "step 5",
        "OK — drain task delivered PortEvent into async-reminder queue",
    );

    // ── step 6: observe PortEvent attachment ──────────────────────────────
    banner(
        "step 6",
        "agent no-op turn; PortEvent attachment should splice onto its input message",
    );
    let reply6 = session
        .step_with_agent_loop(user_input(agent_id, "any events?"))
        .await
        .expect("step 6: step_with_agent_loop must succeed");
    assert_eq!(reply6.turns.len(), 2, "step 6: expected 2 wire turns");

    let port_event = find_attachment(&session, |a| {
        matches!(a, MessageAttachment::PortEvent { .. })
    })
    .expect("step 6: PortEvent attachment must appear in TurnHistory");
    match &port_event {
        MessageAttachment::PortEvent {
            port_id,
            payload,
            at,
            ..
        } => {
            println!("[smoke]   PortEvent attachment: port_id={port_id} at={at} payload={payload}");
            assert_eq!(
                port_id, "mock",
                "step 6: PortEvent.port_id must be 'mock'; got: {port_id}"
            );
            assert_eq!(
                payload["port_marker"].as_str(),
                Some("port-roundtrip-ok"),
                "step 6: PortEvent payload must carry port_marker; got: {payload}"
            );
        }
        _ => unreachable!(),
    }
    banner(
        "step 6",
        "OK — pushed payload made the full round-trip into a spliced PortEvent attachment",
    );

    // ── step 7: capability denial (HTTP not in restricted allowlist) ──────
    // Construct a SECOND session against the same registry but with an
    // allowlist that omits "http". Drive an agent program that calls
    // Port.call "http"; expect ToolOutcome::Error with capability denial.
    // Keep all 15 effects in the row (so the runtime bundle's tag order
    // matches the agent source's tag order — narrowing categories at the
    // preamble level shifts effect tags and breaks dispatch against the
    // full bundle). Per-port gating fires through the resource allowlist:
    // `has_port("http")` returns false because "http" is absent from the
    // Port allowlist, even though the Port category itself is present.
    let denied_caps =
        CapabilitySet::all().with_resources(EffectCategory::Port, [SmolStr::from("mock")]);
    let code_denied_http = format!(
        "_ <- Log.info \"denied http call\"\n\
         resp <- Port.call \"http\" \"get\" \"{{\\\"url\\\":\\\"{url}\\\"}}\"\n\
         pure resp",
        url = http_url
    );
    let denied_scripts: Vec<Vec<genai::chat::ChatStreamEvent>> =
        agent_exchange("toolu_07_denied_http", &code_denied_http, None);
    let denied_provider: Arc<dyn ProviderClient> =
        Arc::new(MockProviderClient::with_turns(denied_scripts));
    let denied_store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let denied_db = test_db().await;
    ensure_agent_row(&denied_db, "agent-denied-http").await;
    let denied_persona = PersonaSnapshot::new("agent-denied-http", "DeniedHttp");
    let denied_sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());
    let denied_session = TidepoolSession::open_with_agent_loop(
        denied_persona,
        &sdk,
        denied_store,
        denied_provider,
        denied_db,
        tokio::runtime::Handle::current(),
        denied_sink,
        None,
        None,
        Some(denied_caps),
        Some(SessionRegistries {
            agent_registry: None,
            router_registry: None,
            wake_registry_extras: None,
            fronting_set: None,
            port_registry: Some(Arc::clone(&registry)),
            file_policy: None, // no file policy needed — denial program doesn't touch files
        }),
    )
    .await
    .expect("step 7: open denied-http session");
    banner(
        "step 7",
        "restricted session: with_resources(Port, [mock]) — agent attempts Port.call \"http\"",
    );
    // Snapshot wiremock's request count BEFORE the denied step so we
    // can prove the agent's call did NOT reach the server.
    let http_calls_before_denial = mock_http.received_requests().await.unwrap().len();
    let _ = denied_session
        .step_with_agent_loop(user_input("agent-denied-http", "try http"))
        .await
        .expect("step 7: step_with_agent_loop runs (the agent call fails, the wire turn succeeds)");
    let denied_results = collect_tool_result_strings(&denied_session);
    let denied_blob = denied_results.join("\n");
    snippet("step 7 tool_result (capability denial)", &denied_blob);
    // POSITIVE assertion: the tool_result must explicitly mention
    // capability denial. Loose `contains("http")` would pass even if
    // the denial path silently broke and the response just echoed the
    // URL; the exact phrase pins the gate's behaviour.
    assert!(
        denied_blob.to_lowercase().contains("capability denied"),
        "step 7: capability-denied tool_result must contain 'capability denied'; got: {denied_blob}"
    );
    // NEGATIVE assertion: the wiremock body marker must NOT appear in
    // the tool_result — proves the call was blocked before reaching
    // HttpPort, not blocked after the fact.
    assert!(
        !denied_blob.contains("http-roundtrip-ok"),
        "step 7: capability-denied tool_result must NOT contain the wiremock body marker; \
         got: {denied_blob}"
    );
    // WIREMOCK assertion: no new request landed on the mock server
    // during the denied step.
    let http_calls_after_denial = mock_http.received_requests().await.unwrap().len();
    assert_eq!(
        http_calls_after_denial,
        http_calls_before_denial,
        "step 7: capability-denied call must NOT reach wiremock; got {} new request(s)",
        http_calls_after_denial - http_calls_before_denial
    );
    banner(
        "step 7",
        "OK — capability denial surfaced in tool_result AND no wiremock request observed",
    );

    // ── step 8: file policy denial (write outside allowed dir) ────────────
    // Use the original session (still has full caps) and have the agent
    // attempt a write to `deny_dir` (which is OUTSIDE its FilePolicy
    // allowlist). The FileManager's default-deny path must surface a
    // PermissionDenied / "no matching rule" error.
    let forbidden_path = deny_dir.path().join("forbidden.txt").display().to_string();
    let code_policy_denied = format!(
        "_ <- Log.info \"denied policy write\"\n\
         File.write \"{path}\" \"should not land\"\n\
         pure (\"did-not-deny\" :: T.Text)",
        path = forbidden_path
    );
    // Append two more turns to the same session's provider script: but
    // the original provider was already constructed and consumed. Open a
    // dedicated session so the scripts don't bleed.
    // Same rationale as step 7: keep the full effect row so tag order
    // aligns with the runtime bundle. The denial we exercise here fires
    // inside the FileManager's policy gate, not in the capability row.
    let policy_caps = CapabilitySet::all();
    let policy_scripts = agent_exchange("toolu_08_denied_policy", &code_policy_denied, None);
    let policy_provider: Arc<dyn ProviderClient> =
        Arc::new(MockProviderClient::with_turns(policy_scripts));
    let policy_store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let policy_db = test_db().await;
    ensure_agent_row(&policy_db, "agent-denied-policy").await;
    let policy_persona = PersonaSnapshot::new("agent-denied-policy", "DeniedPolicy");
    let policy_sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());
    let policy_registry = Arc::new(PortRegistryImpl::new(&tokio::runtime::Handle::current()));
    let policy_session = TidepoolSession::open_with_agent_loop(
        policy_persona,
        &sdk,
        policy_store,
        policy_provider,
        policy_db,
        tokio::runtime::Handle::current(),
        policy_sink,
        None,
        None,
        Some(policy_caps),
        Some(SessionRegistries {
            agent_registry: None,
            router_registry: None,
            wake_registry_extras: None,
            fronting_set: None,
            port_registry: Some(policy_registry),
            // FilePolicy that allows project_dir but the agent writes to
            // deny_dir → default-deny fallthrough.
            file_policy: Some(allow_dir_policy(project_dir.path())),
        }),
    )
    .await
    .expect("step 8: open denied-policy session");
    banner(
        "step 8",
        &format!(
            "policy-denied write: agent attempts File.write to {forbidden_path} (outside FilePolicy allowlist)"
        ),
    );
    let _ = policy_session
        .step_with_agent_loop(user_input("agent-denied-policy", "try forbidden"))
        .await
        .expect("step 8: step_with_agent_loop runs (file write fails, wire turn succeeds)");
    let policy_results = collect_tool_result_strings(&policy_session);
    let policy_blob = policy_results.join("\n");
    snippet("step 8 tool_result (policy denial)", &policy_blob);
    let lc = policy_blob.to_lowercase();
    assert!(
        lc.contains("permissiondenied")
            || lc.contains("permission denied")
            || lc.contains("no matching rule")
            || lc.contains("denied"),
        "step 8: policy-denied tool_result must mention permission denial; got: {policy_blob}"
    );
    assert!(
        !std::path::Path::new(&forbidden_path).exists(),
        "step 8: forbidden file must not have been created on disk: {forbidden_path}"
    );
    banner(
        "step 8",
        "OK — FileManager default-deny path fired and the forbidden file is absent on disk",
    );

    banner("done", "all 8 steps passed end-to-end");

    // ── cleanup ────────────────────────────────────────────────────────────
    drop(session);
    drop(denied_session);
    drop(policy_session);
}

// ── tool_result extractors ──────────────────────────────────────────────────

/// Walk the session's `TurnHistory` and pull every tool_result message's
/// content as a single flat list of strings. Each entry is the raw text
/// content as the model would have seen it (paginated JSON from
/// `paginateResult`, or an error string from `ToolOutcome::Error`).
fn collect_tool_result_strings(session: &TidepoolSession) -> Vec<String> {
    let hist = session.turn_history();
    let guard = hist.lock().expect("turn history lock");
    let mut out = Vec::new();
    for record in guard.iter_active() {
        for msg in &record.output.messages {
            if msg.chat_message.role == genai::chat::ChatRole::Tool {
                out.push(message_text(msg));
            }
        }
        // Inputs can also carry tool_result messages on chained tool_use
        // turns (continuation messages stash the prior turn's tool_result
        // here). Include them defensively.
        for msg in &record.input.messages {
            if msg.chat_message.role == genai::chat::ChatRole::Tool {
                out.push(message_text(msg));
            }
        }
    }
    out
}

/// Walk the session's `TurnHistory` and return the first attachment
/// matching `pred`, cloned.
fn find_attachment(
    session: &TidepoolSession,
    pred: impl Fn(&MessageAttachment) -> bool,
) -> Option<MessageAttachment> {
    let hist = session.turn_history();
    let guard = hist.lock().expect("turn history lock");
    for record in guard.iter_active() {
        for msg in record
            .input
            .messages
            .iter()
            .chain(record.output.messages.iter())
        {
            for a in &msg.attachments {
                if pred(a) {
                    return Some(a.clone());
                }
            }
        }
    }
    None
}

/// Walk the session's `TurnHistory` and return every attachment seen.
fn collect_attachments(session: &TidepoolSession) -> Vec<MessageAttachment> {
    let hist = session.turn_history();
    let guard = hist.lock().expect("turn history lock");
    let mut out = Vec::new();
    for record in guard.iter_active() {
        for msg in record
            .input
            .messages
            .iter()
            .chain(record.output.messages.iter())
        {
            out.extend(msg.attachments.iter().cloned());
        }
    }
    out
}

/// Render a Pattern `Message`'s genai content to a plain string for
/// substring assertions. Combines plain text bodies with tool_call /
/// tool_response part payloads (both serialized via `Value::to_string`)
/// so callers can `.contains(marker)` without caring about the wrapping.
fn message_text(msg: &Message) -> String {
    use genai::chat::ContentPart;
    let mut out = String::new();
    if let Some(joined) = msg.chat_message.content.joined_texts() {
        out.push_str(&joined);
    }
    for part in msg.chat_message.content.parts() {
        match part {
            ContentPart::ToolResponse(tr) => {
                out.push('\n');
                out.push_str(&tr.content.to_string());
            }
            ContentPart::ToolCall(tc) => {
                out.push('\n');
                out.push_str(&tc.fn_arguments.to_string());
            }
            _ => {}
        }
    }
    out
}
