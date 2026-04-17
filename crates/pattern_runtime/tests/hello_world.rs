//! End-to-end integration test: compile a Haskell agent program, JIT it, run it.
//!
//! The agent imports Pattern.Time and Pattern.Log, exercising the runtime inliner
//! (`pattern_runtime::tidepool::inline::inline_sdk_modules`). The inliner flattens
//! those SDK modules into a single combined module before invoking tidepool-extract,
//! avoiding the DataConTable/Core inconsistency that arises from tidepool's
//! multi-module include-path JIT path.
//!
//! The full pipeline exercised: inliner → tidepool-extract → JIT → effect dispatch
//! → value return.

use pattern_runtime::SessionMachine;
use pattern_runtime::sdk::handlers::log::LogHandler;
use pattern_runtime::sdk::handlers::time::TimeHandler;

/// Reduced bundle matching `Eff '[Time, Log]` in hello.hs (unqualified, post-inlining).
/// Effect tag 0 -> Time, tag 1 -> Log.
type HelloBundle = frunk::HList![TimeHandler, LogHandler];

/// End-to-end smoke test using tidepool_runtime::compile_and_run directly with inlined source.
///
/// This test calls `inline_sdk_modules` manually so the direct tidepool path also
/// exercises the inliner — confirming the flattened source is well-formed Haskell.
#[tokio::test]
async fn hello_world_via_tidepool_direct() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let source = include_str!("fixtures/hello.hs");
    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    // Run the inliner so compile_and_run sees a single-module source.
    let module_name = pattern_runtime::tidepool::inline::extract_module_name(source)
        .unwrap_or_else(|| "Hello".to_string());
    let combined =
        pattern_runtime::tidepool::inline::inline_sdk_modules(source, &sdk_dir, &module_name)
            .expect("inliner should succeed");

    let mut bundle: HelloBundle = frunk::hlist![TimeHandler, LogHandler::default()];

    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || tidepool_runtime::compile_and_run(&combined, "agent", &[], &mut bundle, &()))
        .unwrap()
        .join()
        .unwrap();

    let eval_result = result.expect("compile_and_run should succeed");
    let value = eval_result.into_value();

    // Result should be unit ().
    match &value {
        tidepool_eval::value::Value::Con(_, fields) if fields.is_empty() => {
            eprintln!("hello_world_via_tidepool_direct: got unit () as expected");
        }
        other => panic!("expected unit, got: {other:?}"),
    }
}

/// End-to-end test through Pattern's compile_program + SessionMachine wrapper.
#[tokio::test]
async fn hello_world_runs_end_to_end() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let source = include_str!("fixtures/hello.hs");
    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    // Compile. The inliner flattens Pattern.Time + Pattern.Log into the source.
    let program = pattern_runtime::tidepool::compile_program(source, "agent", &sdk_dir)
        .expect("compile hello.hs");

    // Warm the JIT. 64 MiB nursery (matching tidepool's default).
    let mut machine = SessionMachine::new(program, 64 * 1024 * 1024).expect("jit machine");

    // Build reduced bundle: Time at tag 0, Log at tag 1.
    let mut bundle: HelloBundle = frunk::hlist![TimeHandler, LogHandler::default()];
    let user_ctx = ();

    let result = machine.run(&mut bundle, &user_ctx).expect("run");

    // Verify result is Haskell unit () via FromCore round-trip.
    <() as tidepool_bridge::FromCore>::from_value(&result, machine.table())
        .expect("expected unit return from agent");

    eprintln!("hello_world_runs_end_to_end: agent returned unit () successfully");
}
