//! SDK location resolution. Phase 3 implements Directory mode only; Embedded
//! and Auto are declared for API stability but return
//! `RuntimeError::CompileInternal` with guidance to use Directory mode.
//!
//! This module also defines [`FIRST_PARTY_SKILL_DIR`], which is the canonical
//! path to the first-party skill definitions shipped with `pattern_runtime`.
//! It is used by [`pattern_memory::skill::resolve_source_for_path`] to
//! classify loaded `.md` skill files by provenance and assign the correct
//! [`SkillTrustTier`](pattern_core::types::memory_types::SkillTrustTier).

use std::path::PathBuf;

use pattern_core::error::RuntimeError;

/// Absolute path to the first-party skill definitions bundled with
/// `pattern_runtime`.
///
/// The value is baked at build time from `$CARGO_MANIFEST_DIR/resources/skills`
/// using [`concat!`] + [`env!`]. Any `.md` file discovered under this directory
/// is classified as [`SkillSource::SdkResourceDir`] and receives
/// [`SkillTrustTier::FirstParty`], regardless of the `trust_tier` field written
/// in the file's YAML frontmatter.
///
/// [`SkillSource::SdkResourceDir`]: pattern_memory::skill::SkillSource::SdkResourceDir
/// [`SkillTrustTier::FirstParty`]: pattern_core::types::memory_types::SkillTrustTier::FirstParty
pub const FIRST_PARTY_SKILL_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/skills");

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
    /// TODO(post-foundation / SDK-distribution): not yet implemented.
    Embedded,

    /// Disk-first, embedded fallback. `strict: true` requires disk and
    /// embedded contents to match exactly, catching drift.
    ///
    /// TODO(post-foundation / SDK-distribution): not yet implemented.
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
            // Surfacing Err (not panic) lets callers handle the
            // unimplemented variant without unwinding the process —
            // e.g. a CLI can print a clear 'use Directory' hint.
            Self::Embedded => Err(RuntimeError::CompileInternal {
                reason: "SdkLocation::Embedded not yet implemented — \
                         phase: post-foundation SDK-distribution plan. \
                         Use SdkLocation::Directory or the Default \
                         (PATTERN_SDK_DIR env)."
                    .to_string(),
            }),
            // phase: post-foundation SDK-distribution plan; AC2.9-adjacent.
            Self::Auto { .. } => Err(RuntimeError::CompileInternal {
                reason: "SdkLocation::Auto not yet implemented — \
                         phase: post-foundation SDK-distribution plan. \
                         Use SdkLocation::Directory."
                    .to_string(),
            }),
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
    fn embedded_returns_err_not_panic() {
        let loc = SdkLocation::Embedded;
        let err = loc.resolve().unwrap_err();
        match err {
            RuntimeError::CompileInternal { ref reason } => {
                assert!(
                    reason.contains("Embedded not yet implemented"),
                    "reason: {reason}",
                );
            }
            other => panic!("expected CompileInternal, got {other:?}"),
        }
    }

    #[test]
    fn auto_returns_err_not_panic() {
        let loc = SdkLocation::Auto {
            directory: PathBuf::from("/tmp"),
            strict: false,
        };
        let err = loc.resolve().unwrap_err();
        match err {
            RuntimeError::CompileInternal { ref reason } => {
                assert!(
                    reason.contains("Auto not yet implemented"),
                    "reason: {reason}",
                );
            }
            other => panic!("expected CompileInternal, got {other:?}"),
        }
    }
}
