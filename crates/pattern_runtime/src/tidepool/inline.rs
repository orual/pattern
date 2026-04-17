//! Runtime Haskell source preprocessor: flatten Pattern.* SDK imports into one module.
//!
//! Tidepool's JIT does not correctly handle the multi-module case: `-i` include-path
//! support lets `tidepool-extract` parse imports, but the resulting Core expression and
//! DataConTable are inconsistent at JIT time. Cross-module constructor tags are resolved
//! against the wrong table slot, manifesting as `Jit(Yield(Undefined))` CASE TRAP.
//!
//! Tidepool's own `tide` example appears multi-module but actually uses the
//! `haskell_inline!` build-time proc macro, which flattens all included `.hs` files into
//! one module before invoking `tidepool-extract`. Since Pattern agents are dynamically
//! loaded, the macro is unavailable; this module provides the equivalent logic at runtime.
//!
//! # Algorithm
//!
//! 1. Parse agent source for `import Pattern.*` lines. These are the SDK modules to inline.
//! 2. Read each `sdk_dir/Pattern/X.hs` file. Return `InlineError::MissingSdkModule` if absent.
//! 3. Recursively follow `import Pattern.*` lines in included files (cycle-detected).
//! 4. Strip module headers from every file: collect `{-# LANGUAGE … #-}` extensions,
//!    `import` lines, and the body (everything after the header).
//! 5. Exclude imports that reference any module being inlined (prevents cross-module
//!    `import Pattern.X` lines appearing in the combined output).
//! 6. Deduplicate extensions and remaining imports.
//! 7. Emit a single combined module:
//!    ```text
//!    {-# LANGUAGE Ext1, Ext2, ... #-}
//!    module <module_name> where
//!    <deduped external imports>
//!    <concatenated SDK module bodies>
//!    <agent body>
//!    ```
//!
//! # Upstream note
//!
//! Long-term fix: factor this logic into a `tidepool-inline` library crate shared with the
//! `haskell_inline!` macro, and add real multi-module DataConTable support to
//! `tidepool-extract`. Until then, runtime inlining is the correct workaround.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use miette::Diagnostic;
use thiserror::Error;

/// Error returned by [`inline_sdk_modules`].
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum InlineError {
    /// An `import Pattern.X` line references a module with no corresponding
    /// `.hs` file under `sdk_dir/Pattern/X.hs`.
    #[error("SDK module not found: {module}")]
    #[diagnostic(help("ensure {module}.hs exists under the Pattern SDK directory"))]
    MissingSdkModule {
        /// The fully-qualified Haskell module name that is missing (e.g. `Pattern.DoesNotExist`).
        module: String,
    },

    /// An IO failure occurred while reading a source file.
    #[error("failed to read {path}: {source}")]
    ReadFailed {
        /// Path to the file that could not be read.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Flatten an agent program plus its referenced SDK modules into a single
/// Haskell module, matching the preprocessor behaviour of tidepool's
/// `haskell_inline!` build-time macro.
///
/// Current tidepool does not properly support multi-module DataConTable
/// emission: `-i` include paths let tidepool-extract parse imports, but the
/// resulting Core + DataConTable are inconsistent at JIT time (manifests as
/// `Jit(Yield(Undefined))` CASE TRAP). Inlining sidesteps this by
/// presenting tidepool-extract with a single module.
///
/// # Arguments
/// * `agent_source` — the user's Haskell source text, with `module ... where`
///   header and `import Pattern.X` lines.
/// * `sdk_dir` — directory containing `Pattern/` subdir with SDK modules.
/// * `module_name` — name for the combined module (e.g. `"Hello"` derived
///   from `agent_source`'s module declaration).
///
/// # Returns
/// The combined source text, ready to hand to `tidepool_runtime::compile_haskell`.
///
/// # Errors
/// * [`InlineError::MissingSdkModule`] — an `import Pattern.X` line
///   references a module not found under `sdk_dir/Pattern/X.hs`.
/// * [`InlineError::ReadFailed`] — IO failure reading a file.
pub fn inline_sdk_modules(
    agent_source: &str,
    sdk_dir: &Path,
    module_name: &str,
) -> Result<String, InlineError> {
    // Phase 1: discover all Pattern.* modules reachable from the agent source,
    // following transitive imports.  We use BFS with a visited set.
    let agent_imports = extract_pattern_imports(agent_source);
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = agent_imports.into_iter().collect();

    // Pre-populate visited so we don't re-enqueue during BFS.
    for m in &queue {
        visited.insert(m.clone());
    }

    // (module_name → source_text), preserving insertion order for deterministic output.
    let mut sdk_modules: Vec<(String, String)> = Vec::new();

    while let Some(module) = queue.pop_front() {
        let path = sdk_module_path(sdk_dir, &module);
        let content = std::fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                InlineError::MissingSdkModule {
                    module: module.clone(),
                }
            } else {
                InlineError::ReadFailed {
                    path: path.clone(),
                    source: e,
                }
            }
        })?;

        // Discover transitive imports in this SDK module.
        for transitive in extract_pattern_imports(&content) {
            if !visited.contains(&transitive) {
                visited.insert(transitive.clone());
                queue.push_back(transitive);
            }
        }

        sdk_modules.push((module, content));
    }

    // Phase 2: strip headers from all files and combine.
    //
    // `visited` is the set of Pattern.* module names being inlined; we use it to
    // suppress cross-module Pattern.* imports so they don't appear in the output.
    let agent_header = strip_module_header(agent_source);
    let mut all_extensions: Vec<String> = Vec::new();
    let mut all_imports: Vec<String> = Vec::new();
    let mut sdk_body = String::new();

    for (_module_name, content) in &sdk_modules {
        let header = strip_module_header(content);
        merge_extensions(&mut all_extensions, header.extensions);
        merge_imports(&mut all_imports, header.imports, &visited);
        sdk_body.push_str(&header.body);
        sdk_body.push('\n');
    }

    // Agent header is processed last so its extensions/imports are also collected,
    // but the agent body goes at the very end (after SDK bodies).
    merge_extensions(&mut all_extensions, agent_header.extensions);
    merge_imports(&mut all_imports, agent_header.imports, &visited);

    // Phase 3: emit combined source.
    let extensions_line = if all_extensions.is_empty() {
        String::new()
    } else {
        format!("{{-# LANGUAGE {} #-}}\n", all_extensions.join(", "))
    };

    let imports_block = if all_imports.is_empty() {
        String::new()
    } else {
        format!("{}\n", all_imports.join("\n"))
    };

    let combined = format!(
        "{extensions_line}module {module_name} where\n{imports_block}{sdk_body}{}",
        agent_header.body
    );

    Ok(combined)
}

/// Extract the `module X where` name from a Haskell source string.
///
/// Returns `None` if no `module ... where` declaration is found.
/// Strips any export list (parenthesised section between `module` and `where`).
pub fn extract_module_name(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            // Take the token after "module".
            let rest = rest.trim();
            // Module name ends at whitespace, '(', or end of string.
            let name: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '(')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parsed header sections of a Haskell source file.
struct HaskellHeader {
    /// LANGUAGE extension names extracted from `{-# LANGUAGE ... #-}` pragmas.
    extensions: Vec<String>,
    /// Raw `import ...` lines (verbatim, leading/trailing whitespace stripped to one space).
    imports: Vec<String>,
    /// Everything after the header (definitions, type declarations, etc.).
    body: String,
}

/// Strip a Haskell module header, returning collected metadata and the body.
///
/// The "header" consists of:
/// - `{-# LANGUAGE ... #-}` pragma lines → extensions collected.
/// - Other `{-# ... #-}` pragma lines (e.g. OPTIONS_GHC) → silently dropped.
/// - Line comments (`--`) appearing before the `module ... where` declaration
///   (e.g. Haddock file-level documentation) → silently dropped.
/// - `module ... where` declaration (possibly multi-line with export list) →
///   dropped (replaced by the combined module header).
/// - Empty lines before the first non-header line → dropped.
/// - `import ...` lines → collected verbatim.
///
/// Everything from the first non-import, non-pragma, non-module, non-blank,
/// non-comment line after the `module ... where` declaration onward is body.
///
/// Multi-line module declarations are handled with an `in_module_decl` flag:
/// once `module ` is seen, lines are consumed until `where` is found at line end.
fn strip_module_header(source: &str) -> HaskellHeader {
    let mut extensions: Vec<String> = Vec::new();
    let mut imports: Vec<String> = Vec::new();
    let mut body_lines: Vec<&str> = Vec::new();
    let mut past_header = false;
    // True while we're inside a multi-line `module ... where` declaration.
    let mut in_module_decl = false;
    // True once we've seen and consumed the `module ... where` line.
    // Line comments before the module declaration are dropped regardless.
    let mut seen_module = false;

    for line in source.lines() {
        let trimmed = line.trim();

        if !past_header {
            // Continuation of a multi-line module declaration: consume until `where`.
            if in_module_decl {
                if trimmed.ends_with("where") || trimmed == "where" {
                    in_module_decl = false;
                }
                continue;
            }

            // LANGUAGE pragma: extract extension names.
            if trimmed.starts_with("{-#") && trimmed.contains("LANGUAGE") {
                if let Some(start) = trimmed.find("LANGUAGE") {
                    let after = &trimmed[start + "LANGUAGE".len()..];
                    if let Some(end) = after.find("#-}") {
                        for ext in after[..end].split(',') {
                            let ext = ext.trim();
                            if !ext.is_empty() {
                                extensions.push(ext.to_string());
                            }
                        }
                    }
                }
                continue;
            }

            // Other pragmas: skip silently.
            if trimmed.starts_with("{-#") {
                continue;
            }

            // Module declaration: skip this line. If it ends with `where` the
            // declaration is single-line; otherwise we enter multi-line mode.
            if trimmed.starts_with("module ") {
                seen_module = true;
                if !trimmed.ends_with("where") {
                    in_module_decl = true;
                }
                continue;
            }

            // Blank lines in the header section: skip.
            if trimmed.is_empty() {
                continue;
            }

            // Line comments (`--`) that appear before the `module` declaration
            // are file-level Haddock documentation; skip them. After the module
            // declaration, comments start the body (they annotate definitions).
            if !seen_module && trimmed.starts_with("--") {
                continue;
            }

            // Import line: collect verbatim.
            if trimmed.starts_with("import ") {
                imports.push(line.to_string());
                continue;
            }

            // Anything else means the header is over.
            past_header = true;
        }

        body_lines.push(line);
    }

    HaskellHeader {
        extensions,
        imports,
        body: body_lines.join("\n"),
    }
}

/// Map `Pattern.Foo.Bar` → `sdk_dir/Pattern/Foo/Bar.hs`.
fn sdk_module_path(sdk_dir: &Path, module: &str) -> PathBuf {
    // Module name components separated by `.` become path components.
    let rel: PathBuf = module.split('.').collect();
    sdk_dir.join(rel).with_extension("hs")
}

/// Return the `Pattern.*` module names referenced by `import Pattern.*` or
/// `import qualified Pattern.*` lines in `source`.
fn extract_pattern_imports(source: &str) -> Vec<String> {
    let mut result = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("import ") {
            continue;
        }
        // Normalise: remove "qualified".
        let without_qualified = trimmed
            .trim_start_matches("import")
            .trim()
            .trim_start_matches("qualified")
            .trim();
        // The module name is the next whitespace-delimited token.
        let module_token: &str = without_qualified.split_whitespace().next().unwrap_or("");
        if module_token.starts_with("Pattern.") {
            result.push(module_token.to_string());
        }
    }
    result
}

/// Merge `new_exts` into `all`, deduplicating by name.
fn merge_extensions(all: &mut Vec<String>, new_exts: Vec<String>) {
    for ext in new_exts {
        if !all.contains(&ext) {
            all.push(ext);
        }
    }
}

/// Merge `new_imports` into `all`, skipping any that are cross-module Pattern.* references
/// (i.e., references to a module in `inlined_modules`) and deduplicating the rest.
fn merge_imports(
    all: &mut Vec<String>,
    new_imports: Vec<String>,
    inlined_modules: &HashSet<String>,
) {
    for imp in new_imports {
        if is_pattern_import_for_inlined(&imp, inlined_modules) {
            continue;
        }
        let imp_trimmed = imp.trim().to_string();
        if !all.iter().any(|e: &String| e.trim() == imp_trimmed) {
            all.push(imp_trimmed);
        }
    }
}

/// Return `true` if `import_line` imports a `Pattern.*` module that is in `inlined`.
fn is_pattern_import_for_inlined(import_line: &str, inlined: &HashSet<String>) -> bool {
    let trimmed = import_line.trim();
    if !trimmed.starts_with("import ") {
        return false;
    }
    let without_import = trimmed["import ".len()..].trim();
    let without_qualified = without_import.trim_start_matches("qualified").trim();
    let module_token: &str = without_qualified.split_whitespace().next().unwrap_or("");
    inlined.contains(module_token)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ---------------------------------------------------------------------------
    // Helper: build a tiny on-disk SDK tree for testing.
    // ---------------------------------------------------------------------------

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    #[test]
    fn inline_with_no_sdk_imports() {
        let tmp = tempdir();
        let sdk = tmp.path().join("sdk");
        fs::create_dir_all(&sdk).unwrap();

        let source = r#"{-# LANGUAGE OverloadedStrings #-}
module MyAgent where

import Data.Text (Text)

myFunc :: Text -> Text
myFunc x = x
"#;
        let result = inline_sdk_modules(source, &sdk, "MyAgent")
            .expect("no Pattern.* imports; should not error");

        // Module header should use the supplied name.
        assert!(
            result.contains("module MyAgent where"),
            "module header missing:\n{result}"
        );
        // OverloadedStrings should be preserved.
        assert!(
            result.contains("OverloadedStrings"),
            "extension lost:\n{result}"
        );
        // Data.Text import preserved.
        assert!(
            result.contains("import Data.Text"),
            "import lost:\n{result}"
        );
        // Body preserved.
        assert!(result.contains("myFunc x = x"), "body lost:\n{result}");
        // No Pattern.* import should appear.
        assert!(
            !result.contains("import Pattern."),
            "unexpected Pattern.* import:\n{result}"
        );
    }

    #[test]
    fn inline_with_one_sdk_module() {
        let tmp = tempdir();
        let sdk = tmp.path().join("sdk");

        // Write a minimal Pattern.Time SDK module.
        write_file(
            &sdk,
            "Pattern/Time.hs",
            r#"{-# LANGUAGE GADTs #-}
module Pattern.Time where

import Control.Monad.Freer (Eff, Member, send)

data Time a where
  Now :: Time Int

now :: Member Time effs => Eff effs Int
now = send Now
"#,
        );

        let source = r#"{-# LANGUAGE DataKinds, TypeOperators #-}
module Hello where

import qualified Pattern.Time as Time

agent :: Eff '[Time.Time] ()
agent = do
  _t <- Time.now
  return ()
"#;
        let result =
            inline_sdk_modules(source, &sdk, "Hello").expect("Pattern.Time exists; should succeed");

        // Combined module header.
        assert!(
            result.contains("module Hello where"),
            "module header:\n{result}"
        );

        // Extensions from both agent and SDK should be merged.
        assert!(result.contains("GADTs"), "GADTs from SDK lost:\n{result}");
        assert!(
            result.contains("DataKinds"),
            "DataKinds from agent lost:\n{result}"
        );
        assert!(
            result.contains("TypeOperators"),
            "TypeOperators lost:\n{result}"
        );

        // SDK body inlined (Time GADT).
        assert!(
            result.contains("data Time a where"),
            "Time GADT missing:\n{result}"
        );

        // Agent body inlined.
        assert!(
            result.contains("agent :: Eff"),
            "agent body missing:\n{result}"
        );

        // The `import qualified Pattern.Time as Time` should NOT appear in output —
        // Pattern.Time is now inlined.
        assert!(
            !result.contains("import qualified Pattern.Time"),
            "cross-module Pattern.Time import leaked into output:\n{result}"
        );
        assert!(
            !result.contains("import Pattern.Time"),
            "cross-module Pattern.Time import leaked into output:\n{result}"
        );

        // The external import from Pattern.Time (Control.Monad.Freer) should appear.
        assert!(
            result.contains("import Control.Monad.Freer"),
            "Control.Monad.Freer import missing:\n{result}"
        );
    }

    #[test]
    fn inline_with_transitive_imports() {
        let tmp = tempdir();
        let sdk = tmp.path().join("sdk");

        // Pattern.Time — no Pattern.* imports.
        write_file(
            &sdk,
            "Pattern/Time.hs",
            r#"{-# LANGUAGE GADTs #-}
module Pattern.Time where
import Control.Monad.Freer (Eff, Member, send)
data Time a where
  Now :: Time Int
"#,
        );

        // Pattern.Log — no Pattern.* imports.
        write_file(
            &sdk,
            "Pattern/Log.hs",
            r#"{-# LANGUAGE GADTs #-}
module Pattern.Log where
import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
data Log a where
  Info :: Text -> Log ()
"#,
        );

        // Pattern.Prelude — re-exports Time + Log via transitive imports.
        write_file(
            &sdk,
            "Pattern/Prelude.hs",
            r#"module Pattern.Prelude
  ( module Pattern.Time
  , module Pattern.Log
  ) where
import Pattern.Time
import Pattern.Log
"#,
        );

        // Agent only imports Pattern.Prelude.
        let source = r#"module Hello where
import Pattern.Prelude
agent :: ()
agent = ()
"#;

        let result = inline_sdk_modules(source, &sdk, "Hello")
            .expect("transitive imports should be resolved");

        // All three modules must be inlined.
        assert!(
            result.contains("data Time a where"),
            "Time GADT missing:\n{result}"
        );
        assert!(
            result.contains("data Log a where"),
            "Log GADT missing:\n{result}"
        );

        // No Pattern.* imports in the output.
        assert!(
            !result.contains("import Pattern."),
            "residual Pattern.* import:\n{result}"
        );

        // External imports deduplicated (Control.Monad.Freer appears once).
        let count = result.matches("import Control.Monad.Freer").count();
        assert_eq!(
            count, 1,
            "Control.Monad.Freer imported {count} times:\n{result}"
        );
    }

    #[test]
    fn missing_sdk_module_errors() {
        let tmp = tempdir();
        let sdk = tmp.path().join("sdk");
        fs::create_dir_all(&sdk).unwrap();

        let source = r#"module Hello where
import Pattern.DoesNotExist
agent :: ()
agent = ()
"#;

        let err = inline_sdk_modules(source, &sdk, "Hello")
            .expect_err("should fail when SDK module is absent");

        match err {
            InlineError::MissingSdkModule { module } => {
                assert_eq!(module, "Pattern.DoesNotExist", "wrong module: {module}");
            }
            other => panic!("expected MissingSdkModule, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_imports_deduped() {
        let tmp = tempdir();
        let sdk = tmp.path().join("sdk");

        // SDK module that also imports Data.Text.
        write_file(
            &sdk,
            "Pattern/Log.hs",
            r#"{-# LANGUAGE GADTs #-}
module Pattern.Log where
import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
data Log a where
  Info :: Text -> Log ()
"#,
        );

        // Agent also imports Data.Text independently.
        let source = r#"module Hello where
import qualified Pattern.Log as Log
import Data.Text (Text)
agent :: Text -> ()
agent _ = ()
"#;

        let result = inline_sdk_modules(source, &sdk, "Hello").expect("should succeed");

        // Data.Text should appear exactly once.
        let count = result.matches("import Data.Text").count();
        assert_eq!(count, 1, "Data.Text appeared {count} times:\n{result}");
    }

    #[test]
    fn extract_module_name_basic() {
        let src = "module Hello where\nfoo = 1\n";
        assert_eq!(extract_module_name(src), Some("Hello".to_string()));
    }

    #[test]
    fn extract_module_name_with_exports() {
        let src = "module Pattern.Time (Time(..), now) where\ndata Time a where\n";
        assert_eq!(extract_module_name(src), Some("Pattern.Time".to_string()));
    }

    #[test]
    fn extract_module_name_missing() {
        let src = "foo = 1\n";
        assert_eq!(extract_module_name(src), None);
    }

    #[test]
    fn sdk_module_path_maps_dots_to_path() {
        let sdk = Path::new("/sdk");
        let p = sdk_module_path(sdk, "Pattern.Time");
        assert_eq!(p, PathBuf::from("/sdk/Pattern/Time.hs"));
    }

    #[test]
    fn sdk_module_path_nested() {
        let sdk = Path::new("/sdk");
        let p = sdk_module_path(sdk, "Pattern.Foo.Bar");
        assert_eq!(p, PathBuf::from("/sdk/Pattern/Foo/Bar.hs"));
    }

    // ---------------------------------------------------------------------------
    // Helper: create a temp directory (cleaned up on drop).
    // ---------------------------------------------------------------------------

    struct TempDir(PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("pattern_runtime_inline_test_{pid}_{n}"));
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}
