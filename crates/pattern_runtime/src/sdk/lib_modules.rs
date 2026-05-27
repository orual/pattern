// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! `<mount>/lib/` module discovery and per-module probe compilation.
//!
//! Implements Approach A: each `.hs` file under `<mount>/lib/` is
//! probe-compiled individually via `tidepool_runtime::compile_haskell`.
//! The probe source imports the module qualified and calls `pure ()`,
//! exercising GHC's parser and type-checker without executing any
//! effects. Modules that fail the probe are recorded as
//! [`LibCompileFailure`] and surfaced to agents via `Pattern.Diagnostics`;
//! modules that pass cause their containing `lib/` directory to be added
//! to the eval worker's include path.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of validating a mount's `lib/` directory.
///
/// `successful_paths` contains `<mount>/lib/` when at least one module
/// compiled successfully (or when no `.hs` files exist — the directory
/// is still useful as an include path for future imports).
/// `failures` lists per-module compile errors with parsed source
/// locations for `Pattern.Diagnostics`.
#[derive(Debug, Clone, Default)]
pub struct LibValidation {
    /// Directories to append to the eval worker's include path.
    pub successful_paths: Vec<PathBuf>,
    /// Per-module compile failures.
    pub failures: Vec<LibCompileFailure>,
}

/// A record of a per-module compile failure, surfaced to agents via
/// `Pattern.Diagnostics`.
#[derive(Debug, Clone)]
pub struct LibCompileFailure {
    /// Haskell module name (e.g. `"Project.Foo"`).
    pub module_name: String,
    /// Path to the `.hs` source file.
    pub source_path: PathBuf,
    /// Compile error message from Tidepool / GHC.
    pub error_message: String,
    /// Parsed source location (e.g. `"Project/Foo.hs:15:3"`), if extractable.
    pub source_location: Option<String>,
}

// ---------------------------------------------------------------------------
// Module-name inference
// ---------------------------------------------------------------------------

/// Derive a Haskell module name from a `.hs` file path relative to the
/// lib root directory.
///
/// Example: `lib_root = "/m/lib"`, `hs_path = "/m/lib/Project/Foo.hs"`
/// → `Some("Project.Foo")`.
///
/// Returns `None` when the path doesn't live under `lib_root`, has no
/// `.hs` extension, or produces an empty module name.
pub(crate) fn infer_module_name(lib_root: &Path, hs_path: &Path) -> Option<String> {
    let relative = hs_path.strip_prefix(lib_root).ok()?;
    let stem = relative.with_extension("");
    let components: Vec<&str> = stem
        .components()
        .filter_map(|c| {
            if let std::path::Component::Normal(s) = c {
                s.to_str()
            } else {
                None
            }
        })
        .collect();
    if components.is_empty() {
        return None;
    }
    Some(components.join("."))
}

// ---------------------------------------------------------------------------
// Recursive `.hs` discovery
// ---------------------------------------------------------------------------

/// Collect all `.hs` files under `dir` recursively, sorted for
/// deterministic ordering.
fn collect_hs_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_hs_files_inner(dir, &mut files);
    files.sort();
    files
}

fn collect_hs_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_hs_files_inner(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "hs") {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// GHC error location parsing
// ---------------------------------------------------------------------------

/// Extract the first GHC-style source location from a compile error
/// message. Matches patterns like `Project/Foo.hs:15:3` (standard
/// GHC format) or `Project/Foo.hs:(15, 3)` (parenthesized format).
fn extract_source_location(error_message: &str) -> Option<String> {
    // Two regexes: standard colon-separated and parenthesized.
    // We try the standard format first since it's more common.
    static COLON_RE: OnceLock<Regex> = OnceLock::new();
    static PAREN_RE: OnceLock<Regex> = OnceLock::new();

    let colon_re = COLON_RE
        .get_or_init(|| Regex::new(r"([A-Za-z0-9_/]+\.hs):(\d+):(\d+)").expect("static regex"));
    let paren_re = PAREN_RE.get_or_init(|| {
        Regex::new(r"([A-Za-z0-9_/]+\.hs):\((\d+),\s*(\d+)\)").expect("static regex")
    });

    colon_re
        .captures(error_message)
        .or_else(|| paren_re.captures(error_message))
        .map(|caps| {
            format!(
                "{}:{}:{}",
                caps.get(1).unwrap().as_str(),
                caps.get(2).unwrap().as_str(),
                caps.get(3).unwrap().as_str(),
            )
        })
}

// ---------------------------------------------------------------------------
// Probe compilation
// ---------------------------------------------------------------------------

/// Generate a minimal Haskell source that imports a module qualified.
/// GHC's type-checker will reject this if the module has syntax or
/// type errors.
fn probe_source(module_name: &str) -> String {
    format!("module Main where\nimport qualified {module_name}\nmain = pure ()\n")
}

/// Probe-compile a single module. Returns `Ok(())` on success or
/// `Err(LibCompileFailure)` with the GHC error and parsed location.
fn probe_compile_module(
    lib_root: &Path,
    hs_path: &Path,
    module_name: &str,
    include_paths: &[&Path],
) -> Result<(), LibCompileFailure> {
    let source = probe_source(module_name);
    match tidepool_runtime::compile_haskell(&source, "main", include_paths) {
        Ok(_) => Ok(()),
        Err(e) => {
            let error_message = e.to_string();
            let source_location = extract_source_location(&error_message);
            Err(LibCompileFailure {
                module_name: module_name.to_string(),
                source_path: hs_path
                    .strip_prefix(lib_root)
                    .unwrap_or(hs_path)
                    .to_path_buf(),
                error_message,
                source_location,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Probe-compile each `.hs` module under `<mount>/lib/` and return a
/// [`LibValidation`] describing which modules compiled successfully.
///
/// **Approach A:** each module is compiled individually via
/// `tidepool_runtime::compile_haskell` with a minimal probe source
/// (`import qualified <Module>; main = pure ()`). The probe uses the
/// same include paths as the eval worker so that SDK imports
/// (`Pattern.Memory`, etc.) resolve correctly.
///
/// `base_include_paths` should contain the SDK directory (and optional
/// prelude directory) — the same paths the eval worker will receive.
/// The lib directory is appended automatically for the probe.
///
/// If `<mount>/lib/` does not exist, returns an empty [`LibValidation`]
/// (AC14.6: session opens cleanly, no error, no import path extension).
///
/// If `<mount>/lib/` exists but contains no `.hs` files, its path is
/// still added to `successful_paths` (the directory may contain
/// hand-written modules added later, and including an empty directory
/// is harmless).
pub fn validate_and_resolve(mount_path: &Path, base_include_paths: &[PathBuf]) -> LibValidation {
    let lib_dir = mount_path.join("lib");
    if !lib_dir.is_dir() {
        return LibValidation::default();
    }

    let hs_files = collect_hs_files(&lib_dir);

    // No .hs files — still add the lib dir (harmless, forward-compatible).
    if hs_files.is_empty() {
        return LibValidation {
            successful_paths: vec![lib_dir],
            failures: Vec::new(),
        };
    }

    // Build include paths for probe compilation: base paths + lib dir.
    let mut probe_includes: Vec<&Path> = base_include_paths.iter().map(|p| p.as_path()).collect();
    probe_includes.push(lib_dir.as_path());

    let mut any_success = false;
    let mut failures = Vec::new();

    for hs_path in &hs_files {
        let module_name = match infer_module_name(&lib_dir, hs_path) {
            Some(name) => name,
            None => {
                tracing::warn!(
                    path = %hs_path.display(),
                    "could not infer module name from .hs file path; skipping probe"
                );
                continue;
            }
        };

        match probe_compile_module(&lib_dir, hs_path, &module_name, &probe_includes) {
            Ok(()) => {
                any_success = true;
                tracing::debug!(module = %module_name, "lib module probe compiled successfully");
            }
            Err(failure) => {
                tracing::warn!(
                    module = %failure.module_name,
                    error = %failure.error_message,
                    "lib module probe compile failed"
                );
                failures.push(failure);
            }
        }
    }

    // Include the lib dir in the path if at least one module succeeded,
    // or if there were no modules at all (empty-dir case handled above).
    let successful_paths = if any_success {
        vec![lib_dir]
    } else {
        Vec::new()
    };

    LibValidation {
        successful_paths,
        failures,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- Module name inference ------------------------------------------------

    #[test]
    fn infer_module_name_simple() {
        let lib_root = Path::new("/mount/lib");
        let hs_path = Path::new("/mount/lib/Foo.hs");
        assert_eq!(
            infer_module_name(lib_root, hs_path),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn infer_module_name_nested() {
        let lib_root = Path::new("/mount/lib");
        let hs_path = Path::new("/mount/lib/Project/Foo.hs");
        assert_eq!(
            infer_module_name(lib_root, hs_path),
            Some("Project.Foo".to_string())
        );
    }

    #[test]
    fn infer_module_name_deeply_nested() {
        let lib_root = Path::new("/mount/lib");
        let hs_path = Path::new("/mount/lib/A/B/C/D.hs");
        assert_eq!(
            infer_module_name(lib_root, hs_path),
            Some("A.B.C.D".to_string())
        );
    }

    #[test]
    fn infer_module_name_outside_lib_root() {
        let lib_root = Path::new("/mount/lib");
        let hs_path = Path::new("/other/Foo.hs");
        assert_eq!(infer_module_name(lib_root, hs_path), None);
    }

    #[test]
    fn infer_module_name_no_extension_stripped() {
        // Verify .hs extension is stripped properly.
        let lib_root = Path::new("/lib");
        let hs_path = Path::new("/lib/MyModule.hs");
        let name = infer_module_name(lib_root, hs_path).unwrap();
        assert!(!name.contains(".hs"), "should not contain .hs extension");
        assert_eq!(name, "MyModule");
    }

    // -- Recursive .hs discovery -----------------------------------------------

    #[test]
    fn collect_hs_files_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let files = collect_hs_files(tmp.path());
        assert!(files.is_empty());
    }

    #[test]
    fn collect_hs_files_flat() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Foo.hs"), "module Foo where").unwrap();
        std::fs::write(tmp.path().join("Bar.hs"), "module Bar where").unwrap();
        std::fs::write(tmp.path().join("README.md"), "not haskell").unwrap();

        let files = collect_hs_files(tmp.path());
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.extension().unwrap() == "hs"));
    }

    #[test]
    fn collect_hs_files_nested() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("Project");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("Foo.hs"), "module Project.Foo where").unwrap();
        std::fs::write(tmp.path().join("Top.hs"), "module Top where").unwrap();

        let files = collect_hs_files(tmp.path());
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn collect_hs_files_sorted_deterministically() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Z.hs"), "").unwrap();
        std::fs::write(tmp.path().join("A.hs"), "").unwrap();
        std::fs::write(tmp.path().join("M.hs"), "").unwrap();

        let files = collect_hs_files(tmp.path());
        let names: Vec<&str> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["A.hs", "M.hs", "Z.hs"]);
    }

    // -- GHC error location parsing -------------------------------------------

    #[test]
    fn extract_location_standard_format() {
        let msg = "Project/Foo.hs:15:3: error: Not in scope: 'bar'";
        assert_eq!(
            extract_source_location(msg),
            Some("Project/Foo.hs:15:3".to_string())
        );
    }

    #[test]
    fn extract_location_parenthesized_format() {
        let msg = "Project/Foo.hs:(15, 3): error: parse error";
        assert_eq!(
            extract_source_location(msg),
            Some("Project/Foo.hs:15:3".to_string())
        );
    }

    #[test]
    fn extract_location_no_match() {
        let msg = "some generic error without file location";
        assert_eq!(extract_source_location(msg), None);
    }

    // -- Probe source generation -----------------------------------------------

    #[test]
    fn probe_source_generates_valid_haskell() {
        let src = probe_source("Project.Foo");
        assert!(src.contains("import qualified Project.Foo"));
        assert!(src.contains("main = pure ()"));
        assert!(src.starts_with("module Main where"));
    }

    // -- validate_and_resolve (filesystem-level, no tidepool) -----------------

    /// AC14.6: No `lib/` directory -> empty validation, no error.
    #[test]
    fn no_lib_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let result = validate_and_resolve(tmp.path(), &[]);
        assert!(result.successful_paths.is_empty());
        assert!(result.failures.is_empty());
    }

    /// A file named `lib` (not a directory) is not treated as a lib dir.
    #[test]
    fn lib_file_not_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib"), "not a directory").unwrap();

        let result = validate_and_resolve(tmp.path(), &[]);
        assert!(result.successful_paths.is_empty());
    }

    /// Empty lib dir -> path still added (forward-compatible).
    #[test]
    fn empty_lib_dir_adds_path() {
        let tmp = TempDir::new().unwrap();
        let lib_dir = tmp.path().join("lib");
        std::fs::create_dir(&lib_dir).unwrap();

        let result = validate_and_resolve(tmp.path(), &[]);
        assert_eq!(result.successful_paths.len(), 1);
        assert_eq!(result.successful_paths[0], lib_dir);
        assert!(result.failures.is_empty());
    }

    // -- Integration tests gated on tidepool-extract --------------------------

    /// Probe compile of a valid module succeeds.
    #[test]
    fn probe_valid_module_succeeds() {
        if crate::preflight::check().is_err() {
            return;
        }
        let sdk_dir = crate::SdkLocation::default()
            .resolve()
            .expect("SDK dir should resolve");

        let tmp = TempDir::new().unwrap();
        let lib_dir = tmp.path().join("lib");
        let project_dir = lib_dir.join("Project");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("Good.hs"),
            "module Project.Good where\n\ngreet :: String\ngreet = \"hello\"\n",
        )
        .unwrap();

        let result = validate_and_resolve(tmp.path(), &[sdk_dir]);
        assert_eq!(
            result.successful_paths.len(),
            1,
            "lib dir should be in path"
        );
        assert!(result.failures.is_empty(), "no failures expected");
    }

    /// Probe compile of a broken module captures the error.
    #[test]
    fn probe_broken_module_captures_error() {
        if crate::preflight::check().is_err() {
            return;
        }
        let sdk_dir = crate::SdkLocation::default()
            .resolve()
            .expect("SDK dir should resolve");

        let tmp = TempDir::new().unwrap();
        let lib_dir = tmp.path().join("lib");
        std::fs::create_dir(&lib_dir).unwrap();
        std::fs::write(
            lib_dir.join("Broken.hs"),
            "module Broken where\n\nbad :: Int\nbad = \"not an int\"\n",
        )
        .unwrap();

        let result = validate_and_resolve(tmp.path(), &[sdk_dir]);
        // The broken module should be recorded as a failure.
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].module_name, "Broken");
        assert!(!result.failures[0].error_message.is_empty());
        // With only broken modules, the lib dir should NOT be in the path.
        assert!(
            result.successful_paths.is_empty(),
            "no successful modules means no include path"
        );
    }

    /// Mixed valid and broken modules: lib dir is included, broken module
    /// is recorded as failure.
    #[test]
    fn probe_mixed_modules_includes_path_and_records_failures() {
        if crate::preflight::check().is_err() {
            return;
        }
        let sdk_dir = crate::SdkLocation::default()
            .resolve()
            .expect("SDK dir should resolve");

        let tmp = TempDir::new().unwrap();
        let lib_dir = tmp.path().join("lib");
        std::fs::create_dir(&lib_dir).unwrap();

        // Valid module.
        std::fs::write(
            lib_dir.join("Good.hs"),
            "module Good where\n\nvalue :: Int\nvalue = 42\n",
        )
        .unwrap();

        // Broken module.
        std::fs::write(
            lib_dir.join("Bad.hs"),
            "module Bad where\n\nbroken :: Int\nbroken = \"nope\"\n",
        )
        .unwrap();

        let result = validate_and_resolve(tmp.path(), &[sdk_dir]);
        assert_eq!(
            result.successful_paths.len(),
            1,
            "lib dir should be included because Good compiled"
        );
        assert_eq!(result.failures.len(), 1, "Bad should fail");
        assert_eq!(result.failures[0].module_name, "Bad");
    }
}
