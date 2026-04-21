//! Version detection and validation for the jj CLI adapter.
//!
//! Defines the supported version range and helpers for parsing `jj --version`
//! output. The adapter refuses jj versions below [`MIN_SUPPORTED_VERSION`] and
//! logs a warning for versions above [`MAX_TESTED_VERSION`].

use semver::Version;

use super::error::JjError;

/// Minimum jj version Pattern supports. Bump along with [`MAX_TESTED_VERSION`]
/// when regression-testing against a newer jj release.
pub const MIN_SUPPORTED_VERSION: &str = "0.38.0";

/// Most recent jj version Pattern's adapter has been regression-tested against.
/// Versions above this may work but are not guaranteed; a warning is logged.
pub const MAX_TESTED_VERSION: &str = "0.40.0";

/// Parse the version from `jj --version` output.
///
/// jj prints `"jj 0.40.0"` or `"jj 0.40.0-1234-gabcdef"` (with a git-rev
/// suffix on nightly builds). This function strips the `"jj "` prefix and any
/// git-rev suffix, then parses the remaining semver string.
///
/// # Errors
///
/// Returns [`JjError::VersionParse`] if the output does not match the expected
/// format or if the version string is not valid semver.
pub fn parse_jj_version(raw: &str) -> Result<Version, JjError> {
    let token = raw
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| JjError::VersionParse {
            raw: raw.to_owned(),
        })?;
    // Strip any git-rev suffix: "0.40.0-1234-gabcdef" → "0.40.0".
    let clean = token.split('-').next().unwrap_or(token);
    Version::parse(clean).map_err(|_| JjError::VersionParse {
        raw: raw.to_owned(),
    })
}

/// Returns `true` if the version meets the minimum requirement.
pub fn is_supported(v: &Version) -> bool {
    let min = Version::parse(MIN_SUPPORTED_VERSION).expect("static version parses");
    v >= &min
}

/// Returns `true` if the version is within the regression-tested range.
///
/// Versions above [`MAX_TESTED_VERSION`] are still attempted but trigger a
/// warning in the adapter.
pub fn is_tested(v: &Version) -> bool {
    let max = Version::parse(MAX_TESTED_VERSION).expect("static version parses");
    v <= &max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stable_version() {
        let v = parse_jj_version("jj 0.38.0").unwrap();
        assert_eq!(v, Version::new(0, 38, 0));
    }

    #[test]
    fn parse_version_with_git_suffix() {
        let v = parse_jj_version("jj 0.40.0-1234-g0abcdef").unwrap();
        assert_eq!(v, Version::new(0, 40, 0));
    }

    #[test]
    fn parse_current_release() {
        let v = parse_jj_version("jj 0.40.0").unwrap();
        assert_eq!(v, Version::new(0, 40, 0));
    }

    #[test]
    fn parse_garbled_fails() {
        let result = parse_jj_version("garbled");
        assert!(matches!(result, Err(JjError::VersionParse { .. })));
    }

    #[test]
    fn parse_empty_fails() {
        let result = parse_jj_version("");
        assert!(matches!(result, Err(JjError::VersionParse { .. })));
    }

    #[test]
    fn is_supported_below_min() {
        let v = Version::new(0, 37, 0);
        assert!(!is_supported(&v));
    }

    #[test]
    fn is_supported_at_min() {
        let v = Version::new(0, 38, 0);
        assert!(is_supported(&v));
    }

    #[test]
    fn is_supported_above_min() {
        let v = Version::new(0, 40, 0);
        assert!(is_supported(&v));
    }

    #[test]
    fn is_tested_at_max() {
        let v = Version::new(0, 40, 0);
        assert!(is_tested(&v));
    }

    #[test]
    fn is_tested_above_max() {
        let v = Version::new(0, 41, 0);
        assert!(!is_tested(&v));
    }

    #[test]
    fn is_tested_below_max() {
        let v = Version::new(0, 38, 0);
        assert!(is_tested(&v));
    }
}
