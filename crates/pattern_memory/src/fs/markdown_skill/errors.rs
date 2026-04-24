//! Errors surfaced by the skill frontmatter parser.

use miette::SourceSpan;

/// Errors returned by the skill `.md` file parser when it fails to decode
/// a file into a `SkillFile`.
///
/// Each variant carries enough position data to render a useful diagnostic
/// pointing at the offending line/column in the source.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SkillParseError {
    /// File is missing the opening `---\n` or closing `---\n` frontmatter
    /// delimiter.
    #[error("missing frontmatter delimiters (--- ... ---)")]
    MissingDelimiters,

    /// Saphyr failed to parse the frontmatter region as YAML.
    #[error("YAML parse error at {span:?}: {source}")]
    Yaml {
        span: SourceSpan,
        #[source]
        source: saphyr::ScanError,
    },

    /// A required key was absent from the frontmatter mapping.
    #[error("missing required key `{key}`")]
    MissingRequiredKey {
        key: &'static str,
        span: Option<SourceSpan>,
    },

    /// A key's value didn't match the expected YAML kind.
    #[error("key `{key}` has wrong type: expected {expected}, got {actual}")]
    TypeMismatch {
        key: String,
        expected: &'static str,
        actual: &'static str,
        span: Option<SourceSpan>,
    },

    /// `trust_tier` value was a valid string but not one of the four
    /// kebab-case enum names. Distinct from `TypeMismatch` so agents can
    /// detect invalid-enum-value specifically (supports AC7.6).
    #[error(
        "invalid trust tier `{value}` — expected one of: first-party, project-local, plugin-installed, ad-hoc"
    )]
    InvalidTrustTier {
        value: String,
        span: Option<SourceSpan>,
    },

    /// File bytes aren't valid UTF-8.
    #[error("body is not valid UTF-8")]
    NonUtf8Body,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_variants_construct_and_display() {
        // Smoke test: each variant constructs and its Display impl works.
        let err = SkillParseError::MissingDelimiters;
        assert!(err.to_string().contains("---"));

        let err = SkillParseError::MissingRequiredKey {
            key: "name",
            span: Some(SourceSpan::from((0, 4))),
        };
        assert!(err.to_string().contains("name"));

        let err = SkillParseError::InvalidTrustTier {
            value: "foo".to_string(),
            span: None,
        };
        assert!(err.to_string().contains("foo"));
        assert!(err.to_string().contains("first-party")); // lists valid tiers
    }
}
