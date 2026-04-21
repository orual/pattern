//! Reverse-dependency guard: `pattern_core` must never depend on
//! `pattern_memory`. This compile-fail test verifies the dependency
//! boundary by attempting to import `pattern_memory::MemoryCache` and
//! asserting the import fails.

#[test]
fn pattern_core_cannot_import_pattern_memory() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/trybuild/no_pattern_memory_dep.rs");
}
