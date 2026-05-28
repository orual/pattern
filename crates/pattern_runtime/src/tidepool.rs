// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
pub use tidepool_codegen::jit_machine::CancelHandle;
