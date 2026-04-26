//! Integration tests for capability-filtered Haskell compilation.
//!
//! Verifies AC1.2 / AC1.3 (Phase 1, v3-multi-agent): an agent program that
//! references an effect absent from the active [`CapabilitySet`] fails at
//! Tidepool compile time (not at runtime), and a program that references
//! only permitted effects compiles and evaluates normally.
//!
//! Both tests open a full [`TidepoolSession`] with restricted capabilities
//! so the wiring of `open_with_agent_loop` → `build_for` is exercised
//! end-to-end. The session's `preamble()` is then handed to
//! `tidepool_runtime::compile_and_run` along with a `MemoryHandler` +
//! `MessageHandler` bundle aligned with the filtered `type M` row.
//!
//! Gated on `preflight::check()` — skip silently when `tidepool-extract`
//! is not on `$PATH`. CI runs in the Nix devshell where the binary is
//! resolved via `$TIDEPOOL_EXTRACT`.

use std::sync::Arc;

use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::{CapabilitySet, EffectCategory, ProviderClient};
use pattern_runtime::SdkLocation;
use pattern_runtime::sdk::handlers::memory::MemoryHandler;
use pattern_runtime::sdk::handlers::message::MessageHandler;
use pattern_runtime::session::TidepoolSession;
use pattern_runtime::testing::{InMemoryMemoryStore, MockProviderClient};

/// Bundle aligned with the filtered `type M = '[Memory.Memory, Message]`
/// produced for `CapabilitySet::from_iter([Memory, Message])`. Tag 0 →
/// `MemoryHandler`, tag 1 → `MessageHandler`.
type CapBundle = frunk::HList![MemoryHandler, MessageHandler];

/// Materialise the `agents` row required by `messages.agent_id`'s FK.
async fn create_agent_row(db: &pattern_db::ConstellationDb, agent_id: &str) {
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
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).expect("create test agent row");
}

/// Open a session with `caps` active.
async fn open_session_with_caps(agent_id: &str, caps: CapabilitySet) -> TidepoolSession {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
    let db = pattern_runtime::testing::test_db().await;
    create_agent_row(&db, agent_id).await;

    let persona = PersonaSnapshot::new(agent_id, "CapAgent");
    let sdk = SdkLocation::default();
    let sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());

    let port_registry = std::sync::Arc::new(pattern_runtime::port_registry::PortRegistryImpl::new(
        &tokio::runtime::Handle::current(),
    ));
    TidepoolSession::open_with_agent_loop(
        persona,
        &sdk,
        store,
        provider,
        db,
        sink,
        None,
        None,
        Some(caps),
        port_registry,
        None,
    )
    .await
    .expect("open_with_agent_loop should succeed")
}

/// AC1.2: an agent program calling an effect absent from the active
/// capability set fails at Tidepool compile time. The error must
/// reference the missing module name (or otherwise indicate scope
/// resolution failed); it must not surface as a runtime crash.
#[tokio::test]
async fn excluded_effect_fails_at_tidepool_compile() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let caps = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Message]);
    let session = open_session_with_caps("agent-cap-deny", caps).await;

    // Agent calls Shell.execute — `Pattern.Shell` is absent from the
    // session's preamble, so the qualified reference does not resolve.
    let preamble = session
        .preamble()
        .expect("session opened with eval worker must have a preamble")
        .to_string();
    // Raw string so `do`-block indentation survives — Rust's `\n\` line
    // continuation strips leading whitespace, which would collapse the
    // Haskell layout and produce a parse error.
    let body = r#"
agent :: Eff M ()
agent = do
  _ <- Shell.execute "echo hi" 0
  pure ()
"#;
    let source = format!("{preamble}{body}");

    let sdk_dir = SdkLocation::default()
        .resolve()
        .expect("SDK dir must resolve");
    let ctx = session.context();

    // Bundle alignment is irrelevant: compile fails before any handler
    // dispatches, but the type-level row in the agent's source must
    // match a bundle for the call to typecheck on the Rust side.
    let mut bundle: CapBundle = frunk::hlist![MemoryHandler::new(), MessageHandler];

    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tidepool_runtime::compile_and_run(
                &source,
                "agent",
                &[sdk_dir.as_path()],
                &mut bundle,
                &*ctx,
            )
        })
        .expect("spawn")
        .join()
        .expect("thread join");

    let err = result.expect_err("compile must fail when Shell is excluded from the capability set");
    let msg = format!("{err:?}");
    let lc = msg.to_ascii_lowercase();
    // Tidepool's exact wording is upstream-controlled; accept any of the
    // common phrasings for an unresolved name. If upstream phrasing
    // shifts, update this assertion — the *behaviour* (compile-time
    // rejection) is what AC1.2 requires.
    assert!(
        lc.contains("scope")
            || lc.contains("not in scope")
            || lc.contains("undefined")
            || lc.contains("unknown")
            || lc.contains("shell"),
        "expected compile error referencing missing Shell module, got: {msg}"
    );
}

/// AC1.3: an agent program referencing only permitted effects compiles
/// and runs to completion against the bundle that matches the filtered
/// `type M` row.
#[tokio::test]
async fn permitted_effects_compile_and_run() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let caps = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Message]);
    let session = open_session_with_caps("agent-cap-allow", caps).await;

    let preamble = session
        .preamble()
        .expect("session opened with eval worker must have a preamble")
        .to_string();
    // Body uses Memory only. Message is in the row but unused — the
    // compiler doesn't require every effect in the row to be invoked.
    // We avoid Message.send because the session's RouterBridge isn't
    // wired in this test harness; that's covered by message-routing
    // tests elsewhere. Raw string preserves the do-block layout.
    let body = r#"
agent :: Eff M ()
agent = do
  Memory.put "kv" "hello from capability-scoped agent"
  _ <- Memory.get "kv"
  pure ()
"#;
    let source = format!("{preamble}{body}");

    let sdk_dir = SdkLocation::default()
        .resolve()
        .expect("SDK dir must resolve");
    let ctx = session.context();

    let mut bundle: CapBundle = frunk::hlist![MemoryHandler::new(), MessageHandler];

    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tidepool_runtime::compile_and_run(
                &source,
                "agent",
                &[sdk_dir.as_path()],
                &mut bundle,
                &*ctx,
            )
        })
        .expect("spawn")
        .join()
        .expect("thread join");

    let eval_result = result.expect("compile_and_run should succeed when caps match imports");
    let value = eval_result.into_value();
    match &value {
        tidepool_eval::value::Value::Con(_, fields) if fields.is_empty() => {
            // Unit `()` — agent's `pure ()` lowering. AC1.3 verified.
        }
        other => panic!("expected unit constructor, got: {other:?}"),
    }
}
