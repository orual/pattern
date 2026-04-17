//! Validation test: cross-module DataCon name-collision does NOT cause a
//! decode failure after the full fix chain in tidepool-bridge-derive.
//!
//! Two complementary lookup modes defend against module-to-module naming
//! overlaps in the SDK:
//!
//! - **Arity disambiguation** (`get_by_name_arity`) handles constructors
//!   sharing an unqualified name but differing in arity — e.g. a
//!   hypothetical `Foo.Write` (arity 3) vs another module's `Write`
//!   (arity 2).
//! - **Module qualification** (`get_by_qualified_name`, via
//!   `#[core(module = "Pattern.<Module>", name = "...")]`) handles the
//!   residual case where name AND arity collide — e.g. if two modules
//!   both declared `Read :: String -> ...` at the same arity.
//!
//! Pattern's current SDK uses `Memory.Get` / `Memory.Put` (not
//! `Read`/`Write`) and `File.Read` / `File.Write`, so there is no
//! unqualified-name collision at all in practice — this test still
//! exercises the disambiguation layers defensively, and documents the
//! decode contract so a future SDK rename cannot silently regress.
//!
//! Assertions:
//! - No `UnknownDataConQualified`, `UnknownDataConNameArity`, or
//!   `UnknownDataCon` error appears — every DataCon decode succeeds.
//! - The program errors at the dispatch/handler layer (File stub or a
//!   Memory-layer error), which confirms effects routed correctly.
//!
//! STOP condition: any decode-level error signals the module-qualification
//! fix is not reaching the SDK request types.

use std::sync::Arc;

use pattern_core::traits::{AgentRuntime, Session};
use pattern_core::types::ids::new_id;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaConfig;
use pattern_core::types::turn::TurnInput;
use pattern_runtime::TidepoolRuntime;
use pattern_runtime::testing::InMemoryMemoryStore;

fn fresh_turn_input() -> TurnInput {
    TurnInput {
        turn_id: new_id(),
        origin: MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        messages: vec![],
    }
}

/// Core validation: `Memory.Put`, `Memory.Get`, and `File.Read` all
/// dispatch correctly when the agent imports both modules simultaneously.
///
/// The agent (`fixtures/cross_module_collision.hs`) emits three decode
/// events exercising the dispatcher across two modules:
///   1. `M.put "greeting" "hello"` → `Memory.Put` at arity 3 (distinct
///      unqualified name from anything in `File`)
///   2. `M.get "greeting"`         → `Memory.Get` at arity 1 (distinct
///      unqualified name from `File.Read`)
///   3. `F.read_ "/does/not/exist"` → `File.Read` at arity 1 (distinct
///      unqualified name from `Memory.Get`)
///
/// In the current SDK the three constructor names are already distinct,
/// so plain name-based lookup is sufficient. The test is retained as a
/// regression guard: if a future rename reintroduces a collision, the
/// arity-disambiguation + module-qualification layers must continue to
/// resolve correctly.
///
/// Expected outcome: every DataCon decode succeeds. The program surfaces
/// a handler-layer error (Memory.Get fails because the block was never
/// created, or the File stub errors first with "not implemented") — NOT
/// a decode-layer error.
///
/// STOP guard: test panics if any `UnknownDataCon*` variant appears in the
/// error, indicating the fix is not flowing through to the SDK request
/// types.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_and_file_together_do_not_produce_decode_error() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let memory = Arc::new(InMemoryMemoryStore::new());
    let runtime = TidepoolRuntime::with_default_sdk(memory);

    let persona = PersonaConfig::new(
        "collision-agent",
        "CollisionAgent",
        include_str!("fixtures/cross_module_collision.hs"),
    );
    let mut session = runtime
        .open_session(persona, None)
        .await
        .expect("open session");

    let result = session.step(fresh_turn_input()).await;

    match result {
        Ok(_turn_output) => {
            // Unexpectedly succeeded end-to-end. The File stub was supposed to
            // error, but if freer-simple's error handling absorbed it, this is
            // still a valid outcome: decoding definitely worked.
            eprintln!(
                "cross_module_collision: agent completed without error \
                 (stub error may have been absorbed)"
            );
        }
        Err(ref e) => {
            let msg = format!("{e:?}");

            // STOP: any DataCon decode failure is a regression.
            assert!(
                !msg.contains("UnknownDataConQualified"),
                "STOP: module-qualification fix not reaching SDK types — got \
                 UnknownDataConQualified. Verify #[core(module = \"...\")] is \
                 set on every SDK request variant.\nFull error: {msg}"
            );
            assert!(
                !msg.contains("UnknownDataConNameArity"),
                "STOP: arity-disambiguation fix not working — got \
                 UnknownDataConNameArity.\nFull error: {msg}"
            );
            assert!(
                !msg.contains("UnknownDataCon"),
                "STOP: DataCon decode failed — got UnknownDataCon.\n\
                 Full error: {msg}"
            );

            // Acceptable paths: File stub error (handler not implemented),
            // memory errors, or any other non-decode runtime error.
            eprintln!(
                "cross_module_collision: got non-decode error (expected from File stub):\n{msg}"
            );

            // Soft assertion: the error should be traceable to the File effect
            // or the stub, confirming the dispatch reached the handler layer.
            let is_expected_error = msg.contains("Pattern.File")
                || msg.contains("not implemented")
                || msg.contains("Effect")
                || msg.contains("Sdk")
                || msg.contains("Handler");
            assert!(
                is_expected_error,
                "unexpected error type — expected File stub or Effect error, got: {msg}"
            );
        }
    }
}
