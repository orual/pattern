//! Task 19 — AC2.9: every stubbed SDK namespace surfaces its
//! "not implemented" error within a reasonable wall-clock bound.
//!
//! The guarantee we're defending: a phase 3 agent program that calls a
//! stubbed effect does not silently hang. Each handler is wired up
//! enough to reject the request via `EffectError::Handler(..)`, which
//! tidepool raises to the caller as `JitError::Effect(..)`.
//!
//! One sub-test per stubbed namespace (Shell, File, Sources, Mcp, Rpc,
//! Spawn, Message). Each:
//!   1. Compiles and runs a minimal agent invoking one effect in that
//!      namespace against a single-handler bundle.
//!   2. Asserts the call completes under the deadline rather than
//!      hanging.
//!   3. Asserts the returned error text identifies the namespace.
//!
//! Uses `compile_and_run` directly so we observe handler-level errors
//! without the Session error-map layer (which wraps `EffectError` as
//! `SdkError`; either shape is fine for AC2.9's "hang-free" claim).
//!
//! A macro sidesteps per-handler generic-bound boilerplate: the
//! `DispatchEffect` bound for a reduced `HList![H]` bundle varies per
//! handler type and would otherwise require one typed wrapper function
//! per namespace.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pattern_runtime::sdk::handlers::{
    file::FileHandler, mcp::McpHandler, message::MessageHandler, rpc::RpcHandler,
    sources::SourcesHandler, spawn::SpawnHandler,
};

/// Shared per-namespace deadline. The first test across the binary
/// absorbs GHC compile + JIT warm-up cost on a cold cache. Steady-state
/// compiles are ~100ms; 10s is comfortable slack while still failing
/// loudly on a genuine hang.
const STUB_DEADLINE: Duration = Duration::from_secs(10);

fn preflight_or_fail() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");
}

/// Run one stub fixture through `compile_and_run` on a fresh thread with
/// an 8 MiB stack (tidepool JIT needs headroom), enforce the wall-clock
/// deadline, and assert the returned error's Debug rendering contains
/// the two expected substrings.
///
/// Kept as a macro so each invocation instantiates its own
/// `DispatchEffect` impl for the handler type.
///
/// `$user` is the user context passed to `compile_and_run`. Most
/// handlers impl `EffectHandler<U>` generically (bounded on
/// `HasCancelState`), so `&()` works. Handlers bound specifically to
/// `EffectHandler<SessionContext>` (e.g. `MessageHandler` after Task 20
/// part 3 wired the router) require a real `SessionContext`.
macro_rules! run_stub_case {
    ($fixture:expr, $source:expr, $handler:expr, $user:expr, $expect_namespace:expr, $expect_phrase:expr $(,)?) => {{
        let sdk_dir = pattern_runtime::SdkLocation::default()
            .resolve()
            .expect("SDK dir should exist");

        let mut bundle = frunk::hlist![$handler];
        let user = $user;

        let start = Instant::now();
        let result = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let include_path = sdk_dir;
                tidepool_runtime::compile_and_run(
                    $source,
                    "agent",
                    &[include_path.as_path()],
                    &mut bundle,
                    &user,
                )
            })
            .expect("thread spawn")
            .join()
            .expect("thread did not panic");
        let elapsed = start.elapsed();

        assert!(
            elapsed < STUB_DEADLINE,
            "stub fixture {} exceeded deadline {:?}: took {:?}",
            $fixture,
            STUB_DEADLINE,
            elapsed,
        );

        let err = result.expect_err(concat!($fixture, " stub handler should reject its request"));
        let msg = format!("{err:?}");
        assert!(
            msg.contains($expect_namespace) && msg.contains($expect_phrase),
            "expected {} stub error containing {:?} and {:?}, got: {}",
            $fixture,
            $expect_namespace,
            $expect_phrase,
            msg,
        );
    }};
}

// shell_stub_reports_not_implemented_hang_free was removed in Phase 3 Task 6.
// ShellHandler is now a real implementation bound to SessionContext (no longer
// a stub); the AC tests in tests/shell_handler.rs cover the shell surface.

#[test]
fn file_stub_reports_no_file_manager_hang_free() {
    preflight_or_fail();
    run_stub_case!(
        "file_stub",
        include_str!("fixtures/file_read_stub.hs"),
        FileHandler,
        (),
        "Pattern.File",
        "no file manager configured",
    );
}

#[test]
fn sources_stub_reports_not_implemented_hang_free() {
    preflight_or_fail();
    run_stub_case!(
        "sources_stub",
        include_str!("fixtures/sources_stub.hs"),
        SourcesHandler,
        (),
        "Pattern.Sources",
        "not implemented",
    );
}

#[test]
fn mcp_stub_reports_not_implemented_hang_free() {
    preflight_or_fail();
    run_stub_case!(
        "mcp_stub",
        include_str!("fixtures/mcp_stub.hs"),
        McpHandler,
        (),
        "Pattern.Mcp",
        "not implemented",
    );
}

#[test]
fn rpc_stub_reports_not_implemented_hang_free() {
    preflight_or_fail();
    run_stub_case!(
        "rpc_stub",
        include_str!("fixtures/rpc_stub.hs"),
        RpcHandler,
        (),
        "Pattern.Rpc",
        "not implemented",
    );
}

#[test]
fn spawn_stub_reports_not_implemented_hang_free() {
    preflight_or_fail();
    run_stub_case!(
        "spawn_stub",
        include_str!("fixtures/spawn_stub.hs"),
        SpawnHandler,
        (),
        "Pattern.Spawn",
        "not implemented",
    );
}

#[tokio::test]
async fn message_stub_reports_ask_candidate_for_removal_hang_free() {
    preflight_or_fail();
    // Task 20 part 3 wired Send/Reply/Notify to the router registry, so
    // they're no longer stubs. `Ask` remains — stubbed as
    // "candidate-for-removal" per the Task 20 architectural note: v3
    // agents don't call LLMs via effects; LLMs drive agent work via the
    // `code` tool + run_turn, and the reverse doesn't fit. We exercise
    // the Ask path here to defend the AC2.9 "hang-free" claim for the
    // one still-stubbed Message variant.
    //
    // MessageHandler impls EffectHandler<SessionContext> specifically
    // (it reads router + pending_messages from the context), so the
    // test constructs a real SessionContext here rather than passing
    // `&()` like the generically-bound stubs above.
    use pattern_core::ProviderClient;
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::snapshot::PersonaSnapshot;
    use pattern_runtime::NopProviderClient;
    use pattern_runtime::session::SessionContext;
    use pattern_runtime::testing::InMemoryMemoryStore;

    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("agent-a", "A");
    let ctx = SessionContext::from_persona(&persona, store, provider, db);

    run_stub_case!(
        "message_stub",
        include_str!("fixtures/message_stub.hs"),
        MessageHandler,
        ctx,
        "Pattern.Message.Ask",
        "candidate for removal",
    );
}
