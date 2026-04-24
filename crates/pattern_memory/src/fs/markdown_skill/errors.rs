//! Errors surfaced by the skill frontmatter parser.

use miette::{Diagnostic, SourceSpan};

/// Errors returned by the skill `.md` file parser when it fails to decode
/// a file into a `SkillFile`.
///
/// Each variant that carries a span also carries `source_text: String` so
/// that miette's `GraphicalReportHandler` can render the exact offending
/// region with gutter and pointer annotations. The `#[source_code]`
/// attribute tells miette where the source bytes are; `#[label("...")]`
/// annotates the span field with a human-readable pointer message.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum SkillParseError {
    /// File is missing the opening `---\n` or closing `---\n` frontmatter
    /// delimiter.
    #[error("missing frontmatter delimiters (--- ... ---)")]
    MissingDelimiters,

    /// Saphyr failed to parse the frontmatter region as YAML.
    #[error("YAML parse error: {source}")]
    #[diagnostic(code(pattern_memory::skill::yaml_parse_error))]
    Yaml {
        /// Full frontmatter source text — needed by miette to render the span.
        #[source_code]
        source_text: String,
        /// Byte-offset span of the first offending character.
        #[label("invalid YAML here")]
        span: SourceSpan,
        #[source]
        source: saphyr::ScanError,
    },

    /// A required key was absent from the frontmatter mapping.
    #[error("missing required key `{key}`")]
    #[diagnostic(code(pattern_memory::skill::missing_required_key))]
    MissingRequiredKey {
        key: &'static str,
        /// Full frontmatter source text for span rendering.
        #[source_code]
        source_text: String,
        /// Span pointing at the region where the key was expected, if known.
        #[label("key `{key}` not found here")]
        span: Option<SourceSpan>,
    },

    /// A key's value didn't match the expected YAML kind.
    #[error("key `{key}` has wrong type: expected {expected}, got {actual}")]
    #[diagnostic(code(pattern_memory::skill::type_mismatch))]
    TypeMismatch {
        key: String,
        expected: &'static str,
        actual: &'static str,
        /// Full frontmatter source text for span rendering.
        #[source_code]
        source_text: String,
        /// Span pointing at the offending value, if known.
        #[label("wrong type for `{key}` here")]
        span: Option<SourceSpan>,
    },

    /// `trust_tier` value was a valid string but not one of the four
    /// kebab-case enum names. Distinct from `TypeMismatch` so agents can
    /// detect invalid-enum-value specifically (supports AC7.6).
    #[error(
        "invalid trust tier `{value}` — expected one of: first-party, project-local, plugin-installed, ad-hoc"
    )]
    #[diagnostic(code(pattern_memory::skill::invalid_trust_tier))]
    InvalidTrustTier {
        value: String,
        /// Full frontmatter source text for span rendering.
        #[source_code]
        source_text: String,
        /// Span pointing at the offending value, if known.
        #[label("unrecognised tier value here")]
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
            source_text: String::new(),
            span: Some(SourceSpan::from((0, 4))),
        };
        assert!(err.to_string().contains("name"));

        let err = SkillParseError::InvalidTrustTier {
            value: "foo".to_string(),
            source_text: String::new(),
            span: None,
        };
        assert!(err.to_string().contains("foo"));
        assert!(err.to_string().contains("first-party")); // lists valid tiers
    }

    /// Verify that a `miette::Report` wrapping a `Yaml` error renders with
    /// source-location markers and the `#[label]` text when `source_text` is
    /// populated. This proves that the `#[source_code]` + `#[label]`
    /// attributes are correctly wired — not just derived — and that a
    /// terminal operator would actually see the offending span highlighted.
    #[test]
    fn yaml_error_miette_renders_source_highlighting() {
        use miette::GraphicalReportHandler;
        use miette::GraphicalTheme;
        use saphyr::LoadableYamlNode;

        // A two-line YAML snippet with an unclosed bracket on the first line.
        // Having a second line after the error ensures saphyr's error marker
        // is well within the source (not at EOF), so miette can render the
        // `#[label]` annotation with an in-bounds span pointer.
        let frontmatter = "name: [unclosed\nvalid: key\n";
        let docs = saphyr::Yaml::load_from_str(frontmatter);
        let scan_err = match docs {
            Err(e) => e,
            Ok(_) => {
                // If saphyr somehow parses it successfully this test is moot;
                // fail loudly rather than silently vacuously passing.
                panic!(
                    "expected saphyr to fail on unclosed bracket, but it parsed successfully; \
                     update the test input"
                );
            }
        };

        let marker = scan_err.marker();
        let span = SourceSpan::from((marker.index(), 1));

        let err = SkillParseError::Yaml {
            source_text: frontmatter.to_string(),
            span,
            source: scan_err,
        };

        let report = miette::Report::new(err);
        let handler = GraphicalReportHandler::new_themed(GraphicalTheme::none());
        let mut rendered = String::new();
        handler
            .render_report(&mut rendered, report.as_ref())
            .expect("miette render_report must not fail");

        // The error message must appear in the rendered output.
        assert!(
            rendered.contains("YAML parse error"),
            "expected 'YAML parse error' in rendered output; got:\n{rendered}"
        );

        // The offending substring must appear — miette renders the source line
        // containing the span (either `[unclosed` or `name:` from that line).
        assert!(
            rendered.contains("name:")
                || rendered.contains("[unclosed")
                || rendered.contains("valid"),
            "expected the offending source line in rendered output; got:\n{rendered}"
        );

        // A source-location gutter marker confirms that source highlighting is
        // actually active, not just the error message printed on its own.
        // GraphicalReportHandler emits `,-[` or `| ` / `│` markers only when a
        // `SourceCode` is attached and the span is resolvable.
        assert!(
            rendered.contains("| ") || rendered.contains("│") || rendered.contains(",-["),
            "expected a line-number gutter or source-location marker in miette output \
             (source highlighting active); got:\n{rendered}"
        );

        // The `#[label]` text "invalid YAML here" must appear — this is the
        // definitive proof that the attribute is wired, not just derived.
        // (If source_code were missing, miette would print the error message
        // only, with no span pointer and no label text.)
        assert!(
            rendered.contains("invalid YAML here"),
            "expected label text 'invalid YAML here' in rendered output; got:\n{rendered}"
        );
    }
}
