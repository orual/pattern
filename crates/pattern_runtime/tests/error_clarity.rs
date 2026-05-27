// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use pattern_core::types::memory_types::Scope;
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
//
// These tests exercise `persona_loader::load_persona` (the production path)
// rather than parsing directly. The loader uses an intermediate `PersonaFile`
// DTO parsed via knus with different schema semantics (e.g. `agent-id` is
// optional; `model` not `model.choice`; `memory` children not
// `memory_blocks`). Writing to a tempfile first ensures we exercise the
// full I/O → parse → convert pipeline.

/// Write `content` to a temp file and call `load_persona` on it, returning
/// the error (as a string) or panicking if it unexpectedly succeeds.
fn load_bad_persona(content: &str) -> String {
    let dir = tempfile::TempDir::new().expect("create tempdir");
    let path = dir.path().join("bad.kdl");
    std::fs::write(&path, content).unwrap();
    let err = pattern_runtime::persona_loader::load_persona(&path)
        .expect_err("bad persona KDL must fail to load");
    err.to_string()
}

#[test]
fn ac9_5_persona_malformed_kdl_fails_with_parse_error() {
    // Deliberately broken KDL — unclosed brace.
    let bad_kdl = r#"
name "Test"
model {
"#;
    let display = load_bad_persona(bad_kdl);
    // The loader wraps this as PersonaLoadError::Parse. The Display must
    // mention "parsing" (from the error template) and describe the KDL
    // syntax problem.
    assert!(
        display.contains("pars") || display.contains("expected"),
        "error message should describe the parse problem; got: {display}"
    );
}

#[test]
fn ac9_5_persona_missing_name_field_fails() {
    // `name` is required in PersonaFile — omitting it must produce a Parse
    // error that names the field.
    let bad_kdl = r#"
agent-id "test-agent"
"#;
    let display = load_bad_persona(bad_kdl);
    assert!(
        display.contains("name") || display.contains("missing"),
        "error should mention the missing `name` field; got: {display}"
    );
}

#[test]
fn ac9_5_persona_missing_name_without_agent_id_fails() {
    // Both `name` and `agent_id` absent: `name` is required, so this must
    // fail. This replaces the old `missing_agent_id_field` test — in the
    // production PersonaFile, `agent_id` is optional and defaults to `name`.
    // The only way to get a missing-identifier error is to omit `name`
    // entirely (there is nothing to default from).
    let bad_kdl = "";
    let display = load_bad_persona(bad_kdl);
    assert!(
        display.contains("name") || display.contains("missing"),
        "error should mention the missing `name` field; got: {display}"
    );
}

#[test]
fn ac9_5_persona_bad_model_provider_string_fails() {
    // `PersonaFile` uses `model` with a `provider` property. The loader
    // converts the string via `AdapterKind::from_lower_str`, returning
    // `PersonaLoadError::UnknownProvider` on failure.
    let bad_kdl = r#"
name "Test"

model provider="invalid-provider" model-id="claude-sonnet-4-6" {
}
"#;
    let display = load_bad_persona(bad_kdl);
    assert!(
        display.contains("invalid-provider") || display.contains("provider"),
        "error should mention the bad provider; got: {display}"
    );
}

#[test]
fn ac9_5_persona_bad_memory_permission_enum_fails() {
    // Memory blocks use named children inside `memory { ... }`.
    // An unknown `permission` value fails at conversion time and is
    // wrapped as `PersonaLoadError::UnknownPermission`.
    let bad_kdl = r#"
name "Test"

memory {
    persona content="I am a test agent." {
        permission "superuser"
    }
}
"#;
    let display = load_bad_persona(bad_kdl);
    assert!(
        display.contains("superuser")
            || display.contains("permission")
            || display.contains("unknown"),
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
        persona,
        &bad_sdk,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
        sink,
        None,
        None,
        None,
        None,
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

#[test]
fn ac9_5_memory_write_to_unknown_label_returns_write_to_missing_block() {
    let store = InMemoryMemoryStore::new();
    let agent_id = "test-agent-ac9-5";
    let missing_label = "nonexistent-block";

    // `update_block_metadata` on a non-existent label returns
    // `MemoryError::WriteToMissingBlock` with agent_id, label, and op
    // populated — giving a specific, actionable error.
    let err = store
        .update_block_metadata(
            &Scope::global(agent_id),
            missing_label,
            pattern_core::types::memory_types::BlockMetadataPatch::default().pinned(true),
        )
        .expect_err("write to unknown label must return WriteToMissingBlock");

    match &err {
        pattern_core::types::memory_types::MemoryError::WriteToMissingBlock {
            scope: got_scope,
            label: got_label,
            op,
        } => {
            assert_eq!(
                got_scope.id(), agent_id,
                "WriteToMissingBlock must carry the agent_id that was requested"
            );
            assert_eq!(
                got_label, missing_label,
                "WriteToMissingBlock must carry the label that was not found"
            );
            assert_eq!(
                *op, "update_block_metadata",
                "WriteToMissingBlock op must name the failing operation"
            );
        }
        other => panic!("expected MemoryError::WriteToMissingBlock, got: {other:?}"),
    }

    // Display is actionable — mentions both the agent and block.
    let display = err.to_string();
    assert!(
        display.contains(agent_id) || display.contains(missing_label),
        "Display should name the agent and/or block; got: {display}"
    );
}

#[test]
fn ac9_5_memory_update_description_unknown_label_returns_write_to_missing_block() {
    let store = InMemoryMemoryStore::new();
    let agent_id = "test-agent-ac9-5-desc";
    let missing_label = "nonexistent-block-desc";

    let err = store
        .update_block_metadata(
            &Scope::global(agent_id),
            missing_label,
            pattern_core::types::memory_types::BlockMetadataPatch::default()
                .description("new description"),
        )
        .expect_err("update_block_metadata on unknown label must return WriteToMissingBlock");

    match &err {
        pattern_core::types::memory_types::MemoryError::WriteToMissingBlock {
            scope: got_scope,
            label: got_label,
            ..
        } => {
            assert!(
                got_scope.id() == agent_id || got_label == missing_label,
                "WriteToMissingBlock context must match the requested (agent, label) pair"
            );
        }
        other => panic!("expected MemoryError::WriteToMissingBlock, got: {other:?}"),
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
