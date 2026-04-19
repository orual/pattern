//! Cross-module DataCon collision tests.
//!
//! The tests that exercised the multi-module dispatch through the legacy
//! static-program `Session::step` path were retired in Phase 6 Task B alongside
//! that path. The underlying disambiguation mechanism (arity + module-qualified
//! lookup) is already covered by `bundle_non_prelude5.rs` (single-handler
//! `compile_and_run`) and the inline unit tests on the derive layer.
