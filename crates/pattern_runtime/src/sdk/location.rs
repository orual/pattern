//! SDK location resolution. Phase 3 implements Directory mode only; Embedded
//! and Auto are declared for API stability but return a todo! with clear
//! guidance to use Directory mode.

use std::path::PathBuf;

use pattern_core::error::RuntimeError;

/// Where Pattern finds its Haskell SDK modules at runtime.
#[derive(Debug, Clone)]
pub enum SdkLocation {
    /// Read `.hs` files from a directory on disk at runtime.
    ///
    /// The sole Phase 3 implementation. Default path is
    /// `concat!(env!("CARGO_MANIFEST_DIR"), "/haskell")`, overridable via
    /// `PATTERN_SDK_DIR`. Edits to SDK modules take effect on the next
    /// `Session::open` without a Pattern rebuild.
    Directory(PathBuf),

    /// Extract embedded `.hs` files (via `include_str!`) to a temp dir at
    /// Session open. Self-contained distribution; no external files needed.
    ///
    /// TODO: not yet implemented — phase: post-foundation SDK-distribution plan.
    Embedded,

    /// Disk-first, embedded fallback. `strict: true` requires disk and
    /// embedded contents to match exactly, catching drift.
    ///
    /// TODO: not yet implemented — phase: post-foundation SDK-distribution plan.
    Auto {
        /// Path to the on-disk SDK directory.
        directory: PathBuf,
        /// If true, require disk and embedded contents to match byte-for-byte.
        strict: bool,
    },
}

impl Default for SdkLocation {
    fn default() -> Self {
        // Resolve in order: PATTERN_SDK_DIR env override, then CARGO_MANIFEST_DIR baked at build.
        let base = std::env::var("PATTERN_SDK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/haskell")));
        Self::Directory(base)
    }
}

impl SdkLocation {
    /// Resolve to a concrete directory suitable for passing to
    /// `tidepool_runtime::compile_haskell(include=)`.
    pub fn resolve(&self) -> Result<PathBuf, RuntimeError> {
        match self {
            Self::Directory(p) => {
                if !p.is_dir() {
                    return Err(RuntimeError::SdkNotFound {
                        path: p.clone(),
                        hint: "Set PATTERN_SDK_DIR or ensure \
                               crates/pattern_runtime/haskell exists"
                            .into(),
                    });
                }
                Ok(p.clone())
            }
            // phase: post-foundation SDK-distribution plan; AC2.9-adjacent.
            Self::Embedded => todo!(
                "SdkLocation::Embedded not yet implemented — \
                 phase: post-foundation SDK-distribution plan. \
                 Use SdkLocation::Directory or the Default (PATTERN_SDK_DIR env)."
            ),
            // phase: post-foundation SDK-distribution plan; AC2.9-adjacent.
            Self::Auto { .. } => todo!(
                "SdkLocation::Auto not yet implemented — \
                 phase: post-foundation SDK-distribution plan. \
                 Use SdkLocation::Directory."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_resolves_to_existing_haskell_dir() {
        // CARGO_MANIFEST_DIR/haskell was populated by Task 7 (SDK modules).
        let loc = SdkLocation::default();
        let path = loc.resolve().expect("default SDK dir should exist");
        assert!(path.is_dir(), "expected directory at {}", path.display());
        assert!(
            path.ends_with("haskell"),
            "expected path to end with 'haskell', got {}",
            path.display()
        );
    }

    #[test]
    fn non_existent_directory_returns_sdk_not_found() {
        let loc = SdkLocation::Directory(PathBuf::from("/nonexistent/path/to/sdk"));
        let err = loc.resolve().unwrap_err();
        match err {
            RuntimeError::SdkNotFound { ref path, ref hint } => {
                assert!(
                    path.to_str().unwrap().contains("nonexistent"),
                    "path: {path:?}"
                );
                assert!(hint.contains("PATTERN_SDK_DIR"), "hint: {hint}");
            }
            other => panic!("expected SdkNotFound, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "Embedded not yet implemented")]
    fn embedded_panics_with_todo() {
        let loc = SdkLocation::Embedded;
        let _ = loc.resolve();
    }

    #[test]
    #[should_panic(expected = "Auto not yet implemented")]
    fn auto_panics_with_todo() {
        let loc = SdkLocation::Auto {
            directory: PathBuf::from("/tmp"),
            strict: false,
        };
        let _ = loc.resolve();
    }
}
