//! Pattern runtime: Tidepool embedding, agent execution loop, SDK effect handlers.
//!
//! This crate owns the execution machinery that was previously embedded in
//! `pattern_core`. It depends on `pattern_core` only for trait definitions and
//! shared types; it does not re-expose `pattern_core` internals.
//!
//! Populated incrementally across v3 foundation phases 3–5:
//! - Phase 3: Tidepool FFI, timeout harness, SDK effect algebra, agent loop, checkpoint, `time`/`log` handlers.
//! - Phase 5: Memory adapter (wraps preserved storage), pseudo-message emission, pre-turn `current_state` pseudo-turn.

pub mod preflight;
pub mod tidepool;
pub use tidepool::{CompiledProgram, SessionMachine};
