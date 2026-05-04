//! Hook event filter using glob patterns.

use globset::{Glob, GlobMatcher};
use thiserror::Error;

/// A compiled glob filter for matching hook event tags.
///
/// Examples:
/// - `"turn.before"` — literal match
/// - `"task.*"` — any task event
/// - `"task.transitioned.*"` — any task transition variant
/// - `"**"` — firehose (matches every event)
#[derive(Debug, Clone)]
pub struct HookFilter {
    pattern: String,
    matcher: GlobMatcher,
}

impl HookFilter {
    /// Build a filter from a tag glob pattern.
    pub fn new(pattern: impl Into<String>) -> Result<Self, HookFilterError> {
        let pattern = pattern.into();
        let glob = Glob::new(&pattern).map_err(|source| HookFilterError::InvalidGlob {
            pattern: pattern.clone(),
            source,
        })?;
        Ok(Self {
            pattern,
            matcher: glob.compile_matcher(),
        })
    }

    /// Test whether this filter matches the given event tag.
    pub fn matches(&self, tag: &str) -> bool {
        self.matcher.is_match(tag)
    }

    /// The original glob pattern string.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

/// Errors from hook filter compilation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HookFilterError {
    #[error("invalid hook filter pattern {pattern:?}: {source}")]
    InvalidGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
}
