//! Pattern runtime: Tidepool embedding, agent execution loop, SDK effect handlers.
//!
//! This crate owns the execution machinery that was previously embedded in
//! `pattern_core`. It depends on `pattern_core` only for trait definitions and
//! shared types; it does not re-expose `pattern_core` internals.
//!
//! Populated incrementally across v3 foundation phases 3–5:
//! - Phase 3: Tidepool FFI, timeout harness, SDK effect algebra, agent loop, checkpoint, `time`/`log` handlers.
//! - Phase 5: Memory adapter (wraps preserved storage), pseudo-message emission, pre-turn `current_state` pseudo-turn.

pub mod agent_loop;
pub mod checkpoint;
pub mod memory;
pub mod preflight;
pub mod router;
pub mod runtime;
pub mod sdk;
pub mod session;
pub mod tidepool;
pub mod timeout;
pub use runtime::TidepoolRuntime;
pub use sdk::SdkLocation;
pub use session::{SessionContext, TidepoolSession};
pub use tidepool::{CompiledProgram, SessionMachine};

/// Test fixtures re-exported from `tidepool_testing` under Rust-2024-safe
/// paths, plus an in-memory [`pattern_core::traits::MemoryStore`] double
/// (`test_support::InMemoryMemoryStore`) used by session / runtime
/// integration tests.
///
/// The module is compiled unconditionally so integration tests in
/// `crates/pattern_runtime/tests/` can import the helpers; the contents
/// are small enough that the release-binary cost is negligible, and
/// gating this module on a feature flag complicates the workspace's
/// test pipeline. See `testing.rs` for the history of the `gen`
/// submodule workaround (edition 2024 reserves `gen`).
pub mod testing;
pub use testing::NopProviderClient;
