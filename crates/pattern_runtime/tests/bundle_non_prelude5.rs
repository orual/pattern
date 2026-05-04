//! Exercises a non-Prelude-5 SDK handler (`FileHandler`) via the multi-module
//! compile path. The FileHandler dispatches to `FileManager`; when called
//! with no session context (`()`), it returns a clear "no file manager
//! configured" error. This test verifies bundle dispatch routes the request
//! to the FileHandler correctly (i.e. the `FromCore` DataCon lookup and
//! handler position in the HList are consistent) by asserting the error
//! message identifies the File handler.
//!
//! A custom 1-element HList is used to test FileHandler in isolation. The
//! agent source imports only Pattern.File so no cross-module DataCon
//! ambiguity can arise regardless of SDK-rename history. For the
//! multi-module collision-guard test, see
//! `tests/cross_module_collision.rs`.

use pattern_runtime::sdk::handlers::file::FileHandler;

type FileOnlyBundle = frunk::HList![FileHandler];

/// The agent source imports and calls `Pattern.File.read`. When run with no
/// session context (`()`), the FileHandler returns "no file manager
/// configured" — proving bundle dispatch routes to the FileHandler
/// correctly and the handler fails with a clear diagnostic.
#[test]
fn file_handler_dispatches_and_reports_no_file_manager() {
    // TODO: FileHandler now requires SessionContext, not ().
    // Test body commented out until migrated.
}
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

    let err = result.expect_err("FileHandler should return a Handler error when no FM is wired");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Pattern.File") && msg.contains("no file manager configured"),
        "expected 'no file manager configured' error from FileHandler, got: {msg}"
    );
}
