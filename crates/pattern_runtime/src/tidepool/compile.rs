//! Haskell-to-Core compilation wrapper.
//!
//! Provides a single entry-point (`compile_program`) that:
//! 1. Invokes the runtime inliner to flatten Pattern.* SDK imports into one module.
//! 2. Calls `tidepool_runtime::compile_haskell` with no include paths (everything
//!    is already in the combined source).
//! 3. Maps compile errors into `pattern_core::error::RuntimeError`.
//! 4. Packages the output into a `CompiledProgram` ready for JIT compilation by
//!    [`super::machine::SessionMachine`].
//!
//! # Why inlining instead of include paths?
//!
//! Tidepool's `-i` include-path support parses multi-module imports correctly at the
//! extract step, but the resulting Core expression and DataConTable are inconsistent
//! at JIT time. Cross-module constructor tags are resolved against the wrong table
//! slot, manifesting as `Jit(Yield(Undefined))` CASE TRAP. The inliner in
//! [`super::inline`] sidesteps this by presenting `tidepool-extract` with one module.

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
/// top-level binder to extract (e.g., `"agent"`). `sdk_dir` is the directory
/// containing the Pattern SDK `.hs` files (see [`crate::sdk::location`]).
///
/// The source is preprocessed by [`super::inline::inline_sdk_modules`] before
/// being handed to `tidepool_runtime::compile_haskell`. This flattens all
/// `import Pattern.*` SDK modules into a single combined Haskell module, avoiding
/// the multi-module DataConTable inconsistency in the current tidepool JIT.
///
/// Compilation results are cached on disk (via tidepool-runtime's XDG cache) so
/// repeated invocations with identical inputs return quickly.
pub fn compile_program(
    source: &str,
    target: &str,
    sdk_dir: &Path,
) -> Result<CompiledProgram, RuntimeError> {
    // Derive the module name from the source's `module X where` declaration.
    // Fall back to "Agent" if the declaration is absent (unusual but tolerated).
    let module_name =
        super::inline::extract_module_name(source).unwrap_or_else(|| "Agent".to_string());

    // Flatten SDK imports into a single combined module.  Passes an empty
    // include-path list to compile_haskell — everything is already in `combined`.
    let combined =
        super::inline::inline_sdk_modules(source, sdk_dir, &module_name).map_err(|e| {
            RuntimeError::CompileInternal {
                reason: e.to_string(),
            }
        })?;

    let (core, mut data_cons, meta_warnings) =
        tidepool_runtime::compile_haskell(&combined, target, &[])
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
