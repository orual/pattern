//! Summary display rendering for the builder.

use owo_colors::OwoColorize;

/// Renderer for configuration summaries.
pub struct SummaryRenderer {
    lines: Vec<String>,
    width: usize,
}

impl SummaryRenderer {
    /// Create a new summary renderer.
    pub fn new(title: &str) -> Self {
        let width = 100;
        let mut renderer = Self {
            lines: Vec::new(),
            width,
        };
        renderer.top_border(title);
        renderer
    }

    /// Add the top border with title.
    fn top_border(&mut self, title: &str) {
        let title_part = format!("─ {} ", title);
        let remaining = self.width.saturating_sub(title_part.len() + 2);
        let line = format!("╭{}{}╮", title_part, "─".repeat(remaining));
        self.lines.push(line.cyan().to_string());
    }

    /// Add a blank line.
    pub fn blank(&mut self) {
        self.lines.push(self.bordered_line(""));
    }

    /// Add a section header.
    pub fn section(&mut self, name: &str) {
        self.blank();
        self.lines
            .push(self.bordered_line(&name.bold().to_string()));
    }

    /// Add a key-value pair.
    pub fn kv(&mut self, key: &str, value: &str) {
        let formatted = format!("  {:<14} {}", format!("{}:", key), value);
        self.lines.push(self.bordered_line(&formatted));
    }

    /// Add a key-value pair with dimmed value.
    pub fn kv_dimmed(&mut self, key: &str, value: &str) {
        let formatted = format!(
            "  {:<14} {}",
            format!("{}:", key),
            value.dimmed().to_string()
        );
        self.lines.push(self.bordered_line(&formatted));
    }

    /// Add a list item with bullet.
    pub fn list_item(&mut self, text: &str) {
        let formatted = format!("  {} {}", "•".dimmed(), text);
        self.lines.push(self.bordered_line(&formatted));
    }

    /// Add an inline list (comma-separated on one line).
    pub fn inline_list(&mut self, items: &[String]) {
        if items.is_empty() {
            self.lines
                .push(self.bordered_line(&"  (none)".dimmed().to_string()));
        } else {
            let joined = items.join(", ");
            let truncated = if joined.len() > self.width - 8 {
                format!("{}...", &joined[..self.width - 11])
            } else {
                joined
            };
            let formatted = format!("  {}", truncated);
            self.lines.push(self.bordered_line(&formatted));
        }
    }

    /// Add raw text line.
    #[allow(dead_code)]
    pub fn text(&mut self, text: &str) {
        self.lines.push(self.bordered_line(text));
    }

    /// Create a bordered line.
    fn bordered_line(&self, content: &str) -> String {
        // Strip ANSI codes to calculate real width
        let stripped = strip_ansi(content);
        let content_width = stripped.chars().count();
        let padding = self.width.saturating_sub(content_width + 2);

        format!(
            "{} {}{} {}",
            "│".cyan(),
            content,
            " ".repeat(padding),
            "│".cyan()
        )
    }

    /// Finish and render the summary.
    pub fn finish(mut self) -> String {
        self.blank();
        let bottom = format!("╰{}╯", "─".repeat(self.width - 2));
        self.lines.push(bottom.cyan().to_string());
        self.lines.join("\n")
    }
}

/// Strip ANSI escape codes from a string for width calculation.
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut in_escape = false;

    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Truncate a string with ellipsis if too long.
///
/// TODO: fix this to not panic if it splits a unicode character
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Format a file path for display.
pub fn format_path(path: &std::path::Path) -> String {
    format!("(from file: {})", path.display())
        .dimmed()
        .to_string()
}

/// Format an optional value.
pub fn format_optional(value: Option<&str>) -> String {
    value
        .map(|s| truncate(s, 40))
        .unwrap_or_else(|| "(none)".dimmed().to_string())
}

/// Format a count with optional items preview.
#[allow(dead_code)]
pub fn format_count(name: &str, count: usize) -> String {
    format!("{} ({})", name, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn test_strip_ansi() {
        let with_ansi = "\x1b[31mred\x1b[0m text";
        assert_eq!(strip_ansi(with_ansi), "red text");
    }
}
