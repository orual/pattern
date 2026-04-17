//! Haskell-to-Core compilation wrapper.
//!
//! Provides a single entry-point (`compile_program`) that calls `tidepool_runtime::compile_haskell`,
//! maps compile errors into `pattern_core::error::RuntimeError`, and packages the output
//! into a `CompiledProgram` ready for JIT compilation by [`super::machine::SessionMachine`].

use std::path::Path;

use pattern_core::error::RuntimeError;
use tidepool_repr::{CoreExpr, DataConTable};

/// Output of a successful Haskell compilation.
///
/// Holds the GHC Core expression tree and data constructor metadata emitted by
/// `tidepool-extract`. Both are needed by [`super::machine::SessionMachine::new`]
/// to JIT-compile and run the program.
pub struct CompiledProgram {
    /// The GHC Core expression tree extracted from the Haskell source.
    pub core: CoreExpr,
    /// Data constructor metadata required for effect dispatch and heap bridging.
    pub data_cons: DataConTable,
    /// Compiler warnings produced during extraction (informational).
    pub warnings: Vec<String>,
}

/// Compile a Haskell agent program once per session.
///
/// `source` is the full Haskell source text for the agent. `target` is the
/// top-level binder to extract (e.g., `"agent"`). `include_dirs` must contain
/// the Pattern SDK modules (see `sdk::location`).
///
/// Compilation results are cached on disk (via tidepool-runtime's XDG cache) so
/// repeated invocations with identical inputs return quickly.
pub fn compile_program(
    _source: &str,
    _target: &str,
    _include_dirs: &[&Path],
) -> Result<CompiledProgram, RuntimeError> {
    // 1. Call compile_haskell, map CompileError via error_map::map_compile_error.
    // 2. Unpack CompileResult into CompiledProgram.
    // 3. Log warnings via tracing.
    // phase: 3; AC: AC2.1
    todo!("implement per tidepool-runtime::compile_haskell wrapper")
}
