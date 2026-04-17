//! Tidepool FFI boundary.
//!
//! Wraps `tidepool-runtime` and `tidepool-codegen` public APIs into a
//! Pattern-shaped surface: one compile call per session, many run calls per turn,
//! thread-safety assertions, and error-hierarchy translation.
//!
//! Phase 3 focuses on the minimum needed for the agent loop:
//! - `compile::compile_program` — warm a reusable `JitEffectMachine` for a persona
//! - `machine::SessionMachine` — one compiled program, many runs
//! - `error_map::map_compile_error` / `map_jit_error` — central translation point

pub mod compile;
pub mod error_map;
pub mod machine;

pub use compile::{CompiledProgram, compile_program};
pub use machine::SessionMachine;
