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
pub mod compaction;
pub mod file_manager;
pub mod memory;
pub mod permission;
pub mod persona_loader;
pub mod policy;
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
pub use tidepool::CompiledProgram;

/// Test fixtures re-exported from `tidepool_testing` under Rust-2024-safe
/// paths, plus an in-memory [`pattern_core::traits::MemoryStore`] double
/// (`testing::InMemoryMemoryStore`) and scripted [`testing::MockProviderClient`]
/// used by session / runtime integration tests.
///
/// Compiled when:
/// - Running under `cargo test` / `cargo nextest run` (`cfg(test)`) — keeps
///   `crate::testing::*` usable in lib-unit-test modules without opt-in.
/// - The `test-support` feature is enabled — exposes `pattern_runtime::testing`
///   to downstream integration tests and test binaries (e.g. `pattern-test-cli`).
///
/// Production library builds (no feature, not running tests) do not compile
/// this module, preventing test doubles from leaking into release binaries.
///
/// See `testing.rs` for the history of the `gen` submodule workaround
/// (Rust edition 2024 reserves `gen` as a keyword).
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

/// `NopProviderClient` is re-exported at the crate root when `test-support`
/// is active because several binary integration paths reference it without
/// qualifying through `crate::testing`. Gated on the same feature as the
/// module itself.
#[cfg(any(test, feature = "test-support"))]
pub use testing::NopProviderClient;
