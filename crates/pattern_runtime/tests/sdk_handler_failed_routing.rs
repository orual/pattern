//! Verifies that SDK handler failures during a session step surface as
//! `RuntimeError::SdkHandlerFailed` rather than `CompileInternal`.
//!
//! Before the phase-3 review sweep, SDK handler failures were collapsed
//! into `CompileInternal { reason: ... }` alongside genuine substrate /
//! codegen problems. That made callers unable to distinguish a handler
//! that intentionally rejected a request from a runtime misconfiguration
//! without string-scanning the `reason`. The new `SdkHandlerFailed`
//! variant separates those concerns.

use std::sync::Arc;

use pattern_core::error::RuntimeError;
use pattern_core::traits::{AgentRuntime, Session};
use pattern_core::types::ids::{BatchId, new_id};
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::types::turn::TurnInput;
use pattern_runtime::TidepoolRuntime;
use pattern_runtime::testing::{InMemoryMemoryStore, NopProviderClient};

fn fresh_turn_input() -> TurnInput {
    TurnInput {
        turn_id: new_id(),
        batch_id: BatchId::from(new_id()),
        origin: MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        messages: vec![],
    }
}

/// A stub handler (File, in this case) rejects its request via
/// `EffectError::Handler(...)` carrying `"Pattern.File.<op> is not
/// implemented..."`. After the routing fix, the session should surface
/// this as `RuntimeError::SdkHandlerFailed` with `handler` extracted
/// from the message prefix (`"Pattern.File"`) and the full reason
/// preserved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_stub_surface_as_sdk_handler_failed() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory, provider);
    let persona = PersonaSnapshot::new(
        "sdk-fail-routing",
        "SdkFailRouting",
        include_str!("fixtures/file_stub_full_bundle.hs"),
    );
    let mut session = runtime.open_session(persona, None).await.expect("open");

    let err = session
        .step(fresh_turn_input())
        .await
        .expect_err("File stub should reject its request");

    match err {
        RuntimeError::SdkHandlerFailed {
            ref handler,
            ref reason,
        } => {
            assert_eq!(
                handler, "Pattern.File",
                "handler id should be extracted from message prefix; got {handler:?}"
            );
            assert!(
                reason.contains("not implemented"),
                "reason should preserve stub's not-implemented message; got: {reason}",
            );
        }
        // Specifically guard against the pre-fix behaviour.
        RuntimeError::CompileInternal { ref reason } => panic!(
            "regression: SDK handler failure collapsed into CompileInternal; reason = {reason}",
        ),
        other => panic!("expected SdkHandlerFailed, got {other:?}"),
    }
}
