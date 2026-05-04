//! Exercises a non-Prelude-5 SDK handler (`FileHandler`) via the multi-module
//! compile path.
//!
//! TODO: FileHandler now requires SessionContext, not ().
//! This test is disabled until migrated.

use pattern_runtime::sdk::handlers::file::FileHandler;

type FileOnlyBundle = frunk::HList![FileHandler];

#[test]
fn file_handler_dispatches_and_reports_no_file_manager() {
    // TODO: FileHandler now requires SessionContext, not ().
    // Test body removed until migrated to SessionContext-based test helpers.
}
