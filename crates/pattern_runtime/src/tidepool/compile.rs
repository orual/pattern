// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Haskell-to-Core compilation wrapper.
//!
//! Provides a single entry-point (`compile_program`) that:
//! 1. Calls `tidepool_runtime::compile_haskell` with the SDK directory on the
//!    include path so `import Pattern.*` resolves to the on-disk modules.
//! 2. Maps compile errors into `pattern_core::error::RuntimeError`.
//! 3. Packages the output into a `CompiledProgram` ready for JIT compilation by
//!    [`super::machine::SessionMachine`].
//!
//! # Native multi-module compilation
//!
//! Earlier revisions inlined the Pattern.* SDK into a single combined module as a
//! workaround for a DataConTable/CoreExpr inconsistency at JIT time. That bug was
//! fixed upstream (orual/tidepool@6120c51) and verified by
//! `tests/multi_module_sdk.rs`; the inliner path is no longer used.

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
/// containing the Pattern SDK `.hs` files (see [`crate::sdk::location`]); it is
/// passed as a GHC include path so `import Pattern.*` resolves to the on-disk
/// module tree.
///
/// Compilation results are cached on disk (via tidepool-runtime's XDG cache) so
/// repeated invocations with identical inputs return quickly.
pub fn compile_program(
    source: &str,
    target: &str,
    sdk_dir: &Path,
) -> Result<CompiledProgram, RuntimeError> {
    let (core, mut data_cons, meta_warnings) =
        tidepool_runtime::compile_haskell(source, target, &[sdk_dir])
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
