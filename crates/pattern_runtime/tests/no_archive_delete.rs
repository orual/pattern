//! Compile-fail test verifying that `RecallReq::Delete` is no longer
//! reachable via the agent-facing SDK (v3-memory-rework Phase 3, AC4.9).
//!
//! `MemoryStore::delete_archival` is retained in the trait for human-
//! operator tooling (CLI / TUI); agents cannot reach it because the
//! SDK request variant has been removed.

#[test]
fn archive_delete_no_longer_reachable_via_sdk() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/trybuild/no_archive_delete.rs");
}
