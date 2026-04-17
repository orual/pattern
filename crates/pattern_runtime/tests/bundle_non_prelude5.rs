//! Exercises a non-Prelude-5 SDK handler (`FileHandler`) via the multi-module
//! compile path. The FileHandler is stubbed in v3 foundation — it returns
//! `EffectError::Handler("Pattern.File.Read is not implemented ...")` for any
//! File request. This test verifies bundle dispatch routes the request to the
//! FileHandler correctly (i.e. the `FromCore` DataCon lookup and handler
//! position in the HList are consistent) by asserting the error message
//! identifies the File handler.
//!
//! A custom 1-element HList is used to test FileHandler in isolation. The
//! agent source imports only Pattern.File so no DataCon name collisions can
//! arise even for constructors that still share both name and arity with
//! other modules (e.g. `File.Read` and `Memory.Read` are both arity 1).
//! For the multi-module collision validation test, see
//! `tests/cross_module_collision.rs`.

use pattern_runtime::sdk::handlers::file::FileHandler;

type FileOnlyBundle = frunk::HList![FileHandler];

/// The agent source imports and calls `Pattern.File.read_`. The FileHandler is
/// stubbed, so we expect the `compile_and_run` call to surface the handler
/// error message — proving the stub's "not implemented" path is reachable
/// from a multi-module-compiled agent.
#[test]
fn file_handler_stub_reports_not_implemented() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let source = include_str!("fixtures/file_read_stub.hs");
    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    let mut bundle: FileOnlyBundle = frunk::hlist![FileHandler];

    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include_path = sdk_dir;
            tidepool_runtime::compile_and_run(
                source,
                "agent",
                &[include_path.as_path()],
                &mut bundle,
                &(),
            )
        })
        .expect("thread spawn should succeed")
        .join()
        .expect("thread should not panic");

    let err = result.expect_err("FileHandler stub should return a Handler error");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Pattern.File") && msg.contains("not implemented"),
        "expected FileHandler stub message, got: {msg}"
    );
}
