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
    source: &str,
    target: &str,
    include_dirs: &[&Path],
) -> Result<CompiledProgram, RuntimeError> {
    let (core, mut data_cons, meta_warnings) =
        tidepool_runtime::compile_haskell(source, target, include_dirs)
            .map_err(super::error_map::map_compile_error)?;

    // Surface IO-type warnings as hard errors per sandbox policy.
    if meta_warnings.has_io {
        return Err(RuntimeError::SandboxConstraintViolated {
            constraint: pattern_core::error::SandboxConstraint::NoIoAllowed,
            detail: "agent program uses IO types; use SDK effects instead".to_string(),
        });
    }

    // Populate type-sibling groups from case branches so that get_companion
    // can disambiguate constructors sharing unqualified names. Without this,
    // the JIT's case dispatch may fail with CASE TRAP on constructor tags
    // when multiple types share constructor names (e.g. Bin/Tip from Data.Map
    // vs Data.Set). Matches tidepool-runtime's own compile_and_run path.
    data_cons.populate_siblings_from_expr(&core);

    Ok(CompiledProgram {
        core,
        data_cons,
        warnings: Vec::new(),
    })
}
