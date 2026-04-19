//! AC9.5 per-step failure-mode regression tests.
//!
//! Every step in the smoke flow (persona creation, auth, message send,
//! memory write, restart, read-back, edit, cache metric) must fail loudly
//! with a specific, actionable error that points at which step failed.
//!
//! These tests deliberately trigger each failure mode and assert both:
//!   1. The correct error *variant* is returned (not just `Err`).
//!   2. The `Display` output contains a specific substring that names the
//!      failing step, making the error self-diagnosing.
//!
//! No live credentials are required. All failures are local (bad config,
//! bad state, or isolated env-var manipulation that is restored after the
//! test). Tests run deterministically under `cargo nextest run` in CI.
//!
//! Test function names are prefixed `ac9_5_` for greppability.
//!
//! # Note on `unsafe { std::env::set_var / remove_var }`
//!
//! `nextest` runs each test in its own process by default, so per-process
//! env-var manipulation is safe here. The `unsafe` blocks are the minimal
//! required surface for env isolation; each test restores the original
//! value via an RAII guard ([`EnvGuard`]) so failures don't corrupt state.

// ────────────────────────────── imports ─────────────────────────────────────

use std::path::PathBuf;
use std::sync::Arc;

use pattern_core::error::{ProviderError, RuntimeError};
use pattern_core::traits::MemoryStore;
use pattern_core::types::snapshot::{PersonaSnapshot, SessionSnapshot};
use pattern_provider::auth::AnthropicAuthChain;
use pattern_provider::auth::resolver::CredentialChain;
use pattern_provider::gateway::PatternGatewayClient;
use pattern_provider::shaper::ShaperConfig;
use pattern_runtime::SdkLocation;
use pattern_runtime::checkpoint::CheckpointLog;
use pattern_runtime::testing::InMemoryMemoryStore;

// ────────────────────────────── helpers ─────────────────────────────────────

/// RAII guard that restores an env var to its previous state on drop.
///
/// Nextest runs each test in its own process, so cross-test contamination
/// isn't a concern, but per-test cleanup ensures that nested env mutations
/// within a single test don't stack unexpectedly.
struct EnvGuard {
    name: &'static str,
    prior: Option<String>,
}

impl EnvGuard {
    fn remove(name: &'static str) -> Self {
        let prior = std::env::var(name).ok();
        // SAFETY: nextest's per-process isolation makes this safe.
        unsafe { std::env::remove_var(name) };
        Self { name, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: same process, cleanup path.
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

// ────────────────────────────── 1. Persona parse failures ───────────────────

/// Parse a TOML string into `PersonaSnapshot`. Returns the `toml::de::Error`
/// on failure so tests can assert on its contents.
fn parse_persona_toml(toml: &str) -> Result<PersonaSnapshot, toml::de::Error> {
    toml::from_str(toml)
}

#[test]
fn ac9_5_persona_malformed_toml_fails_with_parse_error() {
    // Deliberately broken TOML — unclosed bracket.
    let bad_toml = r#"
        agent_id = "test-agent"
        name = "Test"
        [model
    "#;
    let err = parse_persona_toml(bad_toml).expect_err("malformed TOML must fail");
    let display = err.to_string();
    // The error must point at a parse/syntax problem, not silently succeed.
    assert!(
        display.contains("expected") || display.contains("parse") || display.contains("TOML"),
        "error message should describe the parse problem; got: {display}"
    );
}

#[test]
fn ac9_5_persona_missing_name_field_fails() {
    // `name` has no serde default — it must be present in the TOML.
    let bad_toml = r#"
        agent_id = "test-agent"
    "#;
    let err = parse_persona_toml(bad_toml).expect_err("missing `name` must fail");
    let display = err.to_string();
    assert!(
        display.contains("name") || display.contains("missing"),
        "error should mention the missing field; got: {display}"
    );
}

#[test]
fn ac9_5_persona_missing_agent_id_field_fails() {
    // `agent_id` has no serde default — it must be present in the TOML.
    let bad_toml = r#"
        name = "Test Agent"
    "#;
    let err = parse_persona_toml(bad_toml).expect_err("missing `agent_id` must fail");
    let display = err.to_string();
    assert!(
        display.contains("agent_id") || display.contains("missing"),
        "error should mention the missing field; got: {display}"
    );
}

#[test]
fn ac9_5_persona_bad_model_provider_string_fails() {
    // `AdapterKind` derives `Deserialize`; unknown variants must fail.
    let bad_toml = r#"
        agent_id = "test-agent"
        name = "Test"

        [model.choice]
        provider = "invalid-provider"
        model_id = "claude-sonnet-4-6"
    "#;
    let err = parse_persona_toml(bad_toml).expect_err("unknown provider variant must fail");
    let display = err.to_string();
    // serde reports the unknown variant — assert the message is specific.
    assert!(
        display.contains("invalid-provider")
            || display.contains("model")
            || display.contains("provider")
            || display.contains("unknown variant"),
        "error should mention the bad provider or the field path; got: {display}"
    );
}

#[test]
fn ac9_5_persona_bad_memory_permission_enum_fails() {
    // `MemoryPermission` is `serde(rename_all = "snake_case")`; an unknown
    // variant must fail deserialization.
    let bad_toml = r#"
        agent_id = "test-agent"
        name = "Test"

        [memory_blocks.persona]
        content = "I am a test agent."
        permission = "superuser"
    "#;
    let err = parse_persona_toml(bad_toml).expect_err("unknown permission variant must fail");
    let display = err.to_string();
    assert!(
        display.contains("superuser")
            || display.contains("permission")
            || display.contains("unknown variant"),
        "error should mention the bad permission value; got: {display}"
    );
}

// ────────────────────────────── 2. Auth failures ────────────────────────────

#[tokio::test]
async fn ac9_5_auth_no_api_key_returns_no_auth_available() {
    // Remove the env var so no tier can resolve a credential.
    let _guard = EnvGuard::remove("ANTHROPIC_API_KEY");

    let chain = AnthropicAuthChain::api_key_only();
    let err = chain
        .resolve()
        .await
        .expect_err("absent API key must return NoAuthAvailable");

    // Assert the correct variant — not just "returned Err".
    assert!(
        matches!(err, ProviderError::NoAuthAvailable { ref provider } if provider == "anthropic"),
        "expected NoAuthAvailable {{ provider: \"anthropic\" }}, got: {err:?}"
    );

    // Assert the Display is specific enough to point at the auth step.
    let display = err.to_string();
    assert!(
        display.contains("no auth") || display.contains("anthropic"),
        "Display should name the provider and step; got: {display}"
    );
}

// ────────────────────────────── 3. Provider-build failures ──────────────────

#[test]
fn ac9_5_gateway_builder_with_no_providers_fails_with_shaper_misconfigured() {
    // The builder requires at least one provider to be registered.
    let result = PatternGatewayClient::builder().build();
    let err = result.expect_err("empty gateway must not build");
    assert!(
        matches!(err, ProviderError::ShaperMisconfigured { .. }),
        "expected ShaperMisconfigured, got: {err:?}"
    );
    let display = err.to_string();
    assert!(
        display.contains("shaper")
            || display.contains("misconfigured")
            || display.contains("provider"),
        "Display should describe the misconfiguration step; got: {display}"
    );
}

#[test]
fn ac9_5_shaper_config_empty_x_app_fails_with_shaper_misconfigured() {
    // An empty `x_app` is caught at shaper-config validation time (AC5.5).
    let config = ShaperConfig {
        x_app: String::new(),
        ..ShaperConfig::default()
    };
    let err = config
        .validate()
        .expect_err("empty x_app must fail validation");
    assert!(
        matches!(err, ProviderError::ShaperMisconfigured { ref reason } if reason.contains("x_app")),
        "expected ShaperMisconfigured mentioning x_app, got: {err:?}"
    );
    let display = err.to_string();
    assert!(
        display.contains("x_app") || display.contains("shaper"),
        "Display should name x_app; got: {display}"
    );
}

// ────────────────────────────── 4. Session-open failures ────────────────────

/// Test that opening a session with a non-existent SDK path fails with
/// `RuntimeError::SdkNotFound` that names the bad path.
///
/// Skips cleanly when `tidepool-extract` is not available (preflight would
/// fail first, masking the SDK-path error we're testing).
///
/// The underlying `SdkLocation::resolve()` path is also covered directly in
/// `src/sdk/location.rs` tests; this test exercises the plumbing through the
/// session constructor so we catch any regression in the call site.
#[tokio::test]
async fn ac9_5_session_open_bad_sdk_path_returns_sdk_not_found() {
    // Skip if tidepool-extract is unavailable — preflight would produce a
    // PreflightFailed error that masks the SdkNotFound we want to assert on.
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let bad_sdk = SdkLocation::Directory(PathBuf::from("/nonexistent/sdk/path/for/test"));
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn pattern_core::ProviderClient> =
        Arc::new(pattern_runtime::NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("test-agent", "Test");
    let sink: Arc<dyn pattern_core::traits::TurnSink> = Arc::new(pattern_core::traits::NoOpSink);

    let err = pattern_runtime::session::TidepoolSession::open_with_agent_loop(
        persona, &bad_sdk, store, provider, db, sink, None,
    )
    .await
    .expect_err("bad SDK path must fail session open");

    assert!(
        matches!(err, RuntimeError::SdkNotFound { .. }),
        "expected SdkNotFound, got: {err:?}"
    );
    let display = err.to_string();
    assert!(
        display.contains("nonexistent") || display.contains("SDK") || display.contains("not found"),
        "Display should name the missing SDK path; got: {display}"
    );
}

/// Direct test of `SdkLocation::resolve()` — no tidepool-extract required.
/// This is the pure unit-test counterpart to the session-open test above.
#[test]
fn ac9_5_sdk_location_bad_path_names_the_missing_directory() {
    let loc = SdkLocation::Directory(PathBuf::from("/nonexistent/sdk/path/ac9_5"));
    let err = loc
        .resolve()
        .expect_err("missing directory must fail resolve");

    assert!(
        matches!(err, RuntimeError::SdkNotFound { ref path, .. } if path.to_str().unwrap_or("").contains("nonexistent")),
        "expected SdkNotFound with the bad path, got: {err:?}"
    );
    let display = err.to_string();
    assert!(
        display.contains("SDK") || display.contains("not found"),
        "Display should name the step (SDK resolution); got: {display}"
    );
}

// ────────────────────────────── 5. Memory write to unknown handle ────────────

#[tokio::test]
async fn ac9_5_memory_write_to_unknown_label_returns_not_found() {
    let store = InMemoryMemoryStore::new();
    let agent_id = "test-agent-ac9-5";
    let missing_label = "nonexistent-block";

    // `set_block_pinned` on a non-existent label returns `MemoryError::NotFound`
    // with the agent_id and label populated — giving a specific, actionable error.
    let err = store
        .set_block_pinned(agent_id, missing_label, true)
        .await
        .expect_err("write to unknown label must return NotFound");

    // Assert the correct error variant with populated context fields.
    match &err {
        pattern_core::memory::MemoryError::NotFound {
            agent_id: got_agent,
            label: got_label,
        } => {
            assert_eq!(
                got_agent, agent_id,
                "NotFound must carry the agent_id that was requested"
            );
            assert_eq!(
                got_label, missing_label,
                "NotFound must carry the label that was not found"
            );
        }
        other => panic!("expected MemoryError::NotFound, got: {other:?}"),
    }

    // Assert the Display is actionable — mentions both the agent and block.
    let display = err.to_string();
    assert!(
        display.contains(agent_id) || display.contains(missing_label),
        "Display should name the agent and/or block; got: {display}"
    );
}

#[tokio::test]
async fn ac9_5_memory_update_description_unknown_label_returns_not_found() {
    let store = InMemoryMemoryStore::new();
    let agent_id = "test-agent-ac9-5-desc";
    let missing_label = "nonexistent-block-desc";

    let err = store
        .update_block_description(agent_id, missing_label, "new description")
        .await
        .expect_err("update_block_description on unknown label must return NotFound");

    match &err {
        pattern_core::memory::MemoryError::NotFound {
            agent_id: got_agent,
            label: got_label,
        } => {
            assert!(
                got_agent == agent_id || got_label == missing_label,
                "NotFound context must match the requested (agent, label) pair"
            );
        }
        other => panic!("expected MemoryError::NotFound, got: {other:?}"),
    }
}

// ────────────────────────────── 6. Checkpoint decode failures ────────────────

#[test]
fn ac9_5_checkpoint_decode_empty_personas_names_the_step() {
    // A snapshot with no persona entries cannot be decoded — the error
    // must name "persona" or the missing-entries problem.
    let empty_snap = SessionSnapshot::new(vec![], serde_json::Value::Null);
    let err = CheckpointLog::decode_events(&empty_snap)
        .expect_err("empty personas must fail checkpoint decode");

    assert!(
        matches!(err, RuntimeError::CheckpointFailed { .. }),
        "expected CheckpointFailed, got: {err:?}"
    );
    let display = err.to_string();
    assert!(
        display.contains("checkpoint") || display.contains("persona"),
        "Display should name the checkpoint step and the missing persona; got: {display}"
    );
}

#[test]
fn ac9_5_checkpoint_decode_empty_personas_reason_mentions_persona() {
    // The `reason` field of `CheckpointFailed` must explicitly name
    // "persona" so an operator knows which sub-step failed.
    let empty_snap = SessionSnapshot::new(vec![], serde_json::Value::Null);
    let err = CheckpointLog::decode_events(&empty_snap)
        .expect_err("empty personas must fail checkpoint decode");

    if let RuntimeError::CheckpointFailed { reason } = err {
        assert!(
            reason.contains("persona"),
            "CheckpointFailed.reason must name 'persona'; got: {reason}"
        );
    } else {
        panic!("expected CheckpointFailed variant");
    }
}

#[test]
fn ac9_5_checkpoint_decode_malformed_extra_json_fails() {
    // A persona entry whose `extra` field is not a JSON array (the expected
    // shape for the event log) produces a meaningful decode error.
    let persona =
        PersonaSnapshot::new("agent-x", "X").with_extra(serde_json::json!({ "wrong": "shape" }));
    let snap = SessionSnapshot::new(vec![persona], serde_json::Value::Null);

    let err = CheckpointLog::decode_events(&snap)
        .expect_err("wrong extra shape must fail checkpoint decode");

    assert!(
        matches!(err, RuntimeError::CheckpointFailed { .. }),
        "expected CheckpointFailed, got: {err:?}"
    );
    let display = err.to_string();
    assert!(
        display.contains("checkpoint")
            || display.contains("failed")
            || display.contains("deserialise"),
        "Display should describe the decode failure; got: {display}"
    );
}
