//! Thin wrapper around `tui-markdown` for rendering markdown to ratatui
//! `Text` with syntax highlighting.
//!
//! Provides two functions:
//! - [`render_markdown`] — converts markdown source to owned `Text<'static>`.
//! - [`markdown_height`] — computes the wrapped line count at a given width.

use ratatui::text::{Line, Span, Text};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

/// Render markdown source into an owned `Text<'static>`.
///
/// Uses `tui_markdown::from_str` for parsing and syntax highlighting,
/// then converts all borrowed spans to owned so the result is
/// independent of the source lifetime.
pub fn render_markdown(source: &str) -> Text<'static> {
    let parsed = tui_markdown::from_str(source);
    text_into_owned(parsed)
}

/// Compute the height in terminal lines that the given markdown source
/// would occupy when rendered and wrapped at `width` columns.
///
/// Uses ratatui's `Paragraph::line_count` for accurate wrapping that
/// accounts for word wrap — not a naive `lines.len()`.
pub fn markdown_height(source: &str, width: u16) -> u16 {
    let text = render_markdown(source);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
    (paragraph.line_count(width) as u16).max(1)
}

/// Convert a `Text<'_>` to `Text<'static>` by owning all `Cow::Borrowed`
/// span content.
fn text_into_owned(text: Text<'_>) -> Text<'static> {
    let lines: Vec<Line<'static>> = text
        .lines
        .into_iter()
        .map(|line| {
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|span| Span::styled(span.content.into_owned(), span.style))
                .collect();
            let mut new_line = Line::from(spans);
            new_line.alignment = line.alignment;
            new_line.style = line.style;
            new_line
        })
        .collect();
    let mut new_text = Text::from(lines);
    new_text.alignment = text.alignment;
    new_text.style = text.style;
    new_text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_plain_text() {
        let text = render_markdown("Hello, world!");
        assert!(!text.lines.is_empty(), "should produce at least one line");
        // The rendered text should contain our input.
        let content: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            content.contains("Hello, world!"),
            "rendered text should contain input; got: {content}"
        );
    }

    #[test]
    fn renders_code_block() {
        let source = "Some text\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n";
        let text = render_markdown(source);
        // A fenced code block with 3 lines of code should produce multiple lines.
        assert!(
            text.lines.len() > 1,
            "code block should produce multiple lines; got {}",
            text.lines.len()
        );
    }

    #[test]
    fn markdown_line_count_matches_lines() {
        let source = "Line one\n\nLine two\n\nLine three";
        // At a wide width, no wrapping should occur.
        let height = markdown_height(source, 200);
        let text = render_markdown(source);
        let logical_lines = text.lines.len() as u16;
        // The height from Paragraph should agree with the logical line
        // count when no wrapping is needed.
        assert_eq!(
            height, logical_lines,
            "height ({height}) should match logical lines ({logical_lines}) at wide width"
        );
    }
}
