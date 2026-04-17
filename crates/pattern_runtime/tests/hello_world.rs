//! End-to-end integration test: compile a Haskell agent program, JIT it, run it.
//!
//! This is the first time real Haskell compiles and runs in our runtime.
//! The agent calls `Time.now` then `Log.info` with the result, exercising
//! the full pipeline: tidepool-extract -> JIT -> effect dispatch -> value return.
//!
//! The hello.hs fixture defines effect GADTs inline (not via Pattern.Time/Log
//! import) because tidepool-extract's multi-module include-path compilation
//! currently produces constructor tag mismatches. Constructor names match the
//! Rust-side `FromCore` derivations byte-for-byte, so the same handlers work.

use pattern_runtime::SessionMachine;
use pattern_runtime::sdk::handlers::log::LogHandler;
use pattern_runtime::sdk::handlers::time::TimeHandler;

/// Reduced bundle matching `Eff '[Time, Log]` in hello.hs.
/// Effect tag 0 -> Time, tag 1 -> Log.
type HelloBundle = frunk::HList![TimeHandler, LogHandler];

/// End-to-end smoke test using tidepool_runtime::compile_and_run directly.
#[tokio::test]
async fn hello_world_via_tidepool_direct() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let source = include_str!("fixtures/hello.hs");
    let mut bundle: HelloBundle = frunk::hlist![TimeHandler, LogHandler::default()];

    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || tidepool_runtime::compile_and_run(source, "agent", &[], &mut bundle, &()))
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

    // Compile. No include dirs needed since effect GADTs are inline.
    let program =
        pattern_runtime::tidepool::compile_program(source, "agent", &[]).expect("compile hello.hs");

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
