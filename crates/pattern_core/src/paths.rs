//! Cross-crate root resolution for Pattern's on-disk state.
//!
//! Owns the *resolution* logic — `config_root` / `data_root` /
//! `cache_root` — that every Pattern crate consults. Crate-specific
//! path builders (e.g. [`pattern_memory::PatternPaths::standalone_mount_path`])
//! wrap a [`PatternRoots`] and add their own subdir conventions on top.
//!
//! # Roots
//!
//! - **`config_root`** — `dirs::config_dir().join("pattern")`. User
//!   credentials and other user-editable config.
//! - **`data_root`** — `dirs::data_dir().join("pattern")`. Standalone
//!   mounts, message databases, backups, personas, project registry,
//!   daemon runtime state.
//! - **`cache_root`** — `dirs::cache_dir().join("pattern")`. Reserved
//!   for plugin caches; currently unused.
//!
//! On macOS and Windows, `dirs::config_dir`, `dirs::data_dir`, and
//! `dirs::cache_dir` may resolve to the same parent directory. That's
//! the platform convention — Pattern's inner subdirs (`creds/`,
//! `projects/`, `daemon/`) keep contents logically distinct even when
//! physically colocated.
//!
//! # `PATTERN_HOME` override
//!
//! Setting `$PATTERN_HOME=<base>` collapses all three roots to
//! `<base>/{config,data,cache}/`. Useful for parallel daemon instances,
//! integration-test isolation, and pinning Pattern's state under a
//! single directory. When set, the platform `dirs::*` values are
//! ignored entirely.

use std::path::{Path, PathBuf};

/// Errors produced by root resolution.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum RootsError {
    /// `dirs::config_dir()` returned `None` and `$PATTERN_HOME` was unset.
    #[error("no config directory available (and $PATTERN_HOME not set)")]
    #[diagnostic(code(pattern_core::paths::no_config_dir))]
    NoConfigDir,

    /// `dirs::data_dir()` returned `None` and `$PATTERN_HOME` was unset.
    #[error("no data directory available (and $PATTERN_HOME not set)")]
    #[diagnostic(code(pattern_core::paths::no_data_dir))]
    NoDataDir,

    /// `dirs::cache_dir()` returned `None` and `$PATTERN_HOME` was unset.
    #[error("no cache directory available (and $PATTERN_HOME not set)")]
    #[diagnostic(code(pattern_core::paths::no_cache_dir))]
    NoCacheDir,
}

/// The three platform-conventional roots Pattern stores files under.
///
/// Construct via [`PatternRoots::default_paths`] for production
/// resolution (XDG-aware on Linux, platform-conventional on macOS /
/// Windows, with `$PATTERN_HOME` as override) or
/// [`PatternRoots::with_base`] for tests.
#[derive(Debug, Clone)]
pub struct PatternRoots {
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
}

impl PatternRoots {
    /// Resolve the three roots from the environment.
    ///
    /// Resolution order:
    /// 1. If `$PATTERN_HOME=<base>` is set and non-empty, all three
    ///    roots collapse to `<base>/{config,data,cache}/`.
    /// 2. Otherwise: each root is `dirs::<kind>_dir().join("pattern")`.
    pub fn default_paths() -> Result<Self, RootsError> {
        if let Some(home) = std::env::var_os("PATTERN_HOME").filter(|s| !s.is_empty()) {
            return Ok(Self::pile_under(Path::new(&home)));
        }
        Ok(Self {
            config: dirs::config_dir()
                .ok_or(RootsError::NoConfigDir)?
                .join("pattern"),
            data: dirs::data_dir()
                .ok_or(RootsError::NoDataDir)?
                .join("pattern"),
            cache: dirs::cache_dir()
                .ok_or(RootsError::NoCacheDir)?
                .join("pattern"),
        })
    }

    /// Pile all three roots under a single base directory:
    /// `<base>/{config,data,cache}/`.
    ///
    /// Identical shape to `PATTERN_HOME=<base>`. Intended for tests —
    /// callers pass a `TempDir` path to isolate from real user state.
    pub fn with_base(base: impl Into<PathBuf>) -> Self {
        Self::pile_under(&base.into())
    }

    fn pile_under(base: &Path) -> Self {
        Self {
            config: base.join("config"),
            data: base.join("data"),
            cache: base.join("cache"),
        }
    }

    /// Root for user-editable configuration (e.g. credentials).
    pub fn config_root(&self) -> &Path {
        &self.config
    }

    /// Root for durable user data (mounts, messages, backups, personas,
    /// daemon state, project registry).
    pub fn data_root(&self) -> &Path {
        &self.data
    }

    /// Root for regenerable caches. Reserved for plugin caches.
    pub fn cache_root(&self) -> &Path {
        &self.cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn with_base_piles_three_roots() {
        let tmp = TempDir::new().unwrap();
        let roots = PatternRoots::with_base(tmp.path());
        assert_eq!(roots.config_root(), tmp.path().join("config"));
        assert_eq!(roots.data_root(), tmp.path().join("data"));
        assert_eq!(roots.cache_root(), tmp.path().join("cache"));
    }
}
