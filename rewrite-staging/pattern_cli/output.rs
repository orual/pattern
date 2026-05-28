use owo_colors::OwoColorize;
use rustyline_async::SharedWriter;
use std::io::Write;
use termimad::MadSkin;

/// Standard output formatting for the CLI
#[derive(Clone)]
pub struct Output {
    skin: MadSkin,
    reasoning_skin: MadSkin,
    writer: Option<SharedWriter>,
}

impl Output {
    pub fn new() -> Self {
        let mut skin = MadSkin::default();
        // Keep it simple for copy-paste friendliness
        skin.set_headers_fg(termimad::ansi(6)); // cyan
        skin.bold.set_fg(termimad::ansi(15)); // bright white

        // Make inline code stand out with color but no background
        skin.inline_code.set_fg(termimad::ansi(11)); // bright yellow
        skin.inline_code
            .set_bg(termimad::crossterm::style::Color::Black);

        // Fix code blocks to not have background
        skin.code_block
            .set_bg(termimad::crossterm::style::Color::Black);
        skin.code_block.set_fg(termimad::ansi(15)); // bright white text
        // If a global SharedWriter is available (when chat has initialized
        // the readline UI), use it to avoid cursor glitches
        let mut reasoning_skin = MadSkin::default();
        // Keep it simple for copy-paste friendliness
        reasoning_skin.set_headers_fg(termimad::ansi(5)); // purple
        reasoning_skin.bold.set_fg(termimad::ansi(7)); //  light grey

        // Make inline code stand out with color but no background
        reasoning_skin.inline_code.set_fg(termimad::ansi(11)); // bright yellow
        reasoning_skin
            .inline_code
            .set_bg(termimad::crossterm::style::Color::Black);

        // Fix code blocks to not have background
        reasoning_skin
            .code_block
            .set_bg(termimad::crossterm::style::Color::Black);
        reasoning_skin.code_block.set_fg(termimad::ansi(7)); // bright white
        // If a global SharedWriter is available (when chat has initialized
        // the readline UI), use it to avoid cursor glitches
        if let Some(shared) = crate::tracing_writer::get_shared_writer() {
            Self {
                skin,
                reasoning_skin,
                writer: Some(shared),
            }
        } else {
            Self {
                skin,
                reasoning_skin,
                writer: None,
            }
        }
    }

    pub fn with_writer(self, writer: SharedWriter) -> Self {
        Self {
            skin: self.skin,
            reasoning_skin: self.reasoning_skin,
            writer: Some(writer),
        }
    }

    /// Helper method to write output either to SharedWriter or stdout
    fn write_line(&self, content: &str) {
        if let Some(ref writer) = self.writer {
            // Clone the writer to get a mutable version
            let mut writer = writer.clone();
            // When using SharedWriter, it handles the synchronization
            let _ = writeln!(writer, "{}", content);
            // Force flush to ensure immediate output
            let _ = writer.flush();
        } else {
            // Fallback to regular println
            println!("{}", content);
            // Force flush stdout for immediate output
            use std::io::{self, Write};
            let _ = io::stdout().flush();
        }
    }

    /// flush the buffer
    fn flush(&self) {
        if let Some(ref writer) = self.writer {
            // Clone the writer to get a mutable version
            let mut writer = writer.clone();
            let _ = writer.flush();
        } else {
            use std::io::{self, Write};
            let _ = io::stdout().flush();
        }
    }

    /// Helper method to write output either to SharedWriter or stdout
    fn write_line_noclear(&self, content: &str) {
        if let Some(ref writer) = self.writer {
            // Clone the writer to get a mutable version
            let mut writer = writer.clone();
            // When using SharedWriter, it handles the synchronization
            let _ = writeln!(writer, "{}", content);
        } else {
            // Fallback to regular println
            println!("{}", content);
        }
    }

    /// Print an agent message with markdown formatting
    pub fn agent_message(&self, agent_name: &str, content: &str) {
        // Clear visual separation without box drawing chars
        self.write_line_noclear("");
        self.write_line_noclear(&format!(
            "{} {}",
            agent_name.bright_cyan().bold(),
            "says:".dimmed()
        ));

        // Use termimad to format the markdown content
        use termimad::FmtText;
        let formatted = FmtText::from(&self.skin, content, Some(80));
        let formatted_string = formatted.to_string();

        // Write each line through our write_line method
        for line in formatted_string.lines() {
            self.write_line_noclear(line);
        }

        self.write_line("");
    }

    /// Print an agent message with markdown formatting
    pub fn agent_reasoning(&self, agent_name: &str, content: &str) {
        self.write_line_noclear(&format!(
            "{} reasoning:",
            agent_name.bright_magenta().bold()
        ));

        // Use termimad to format the markdown content
        use termimad::FmtText;
        let formatted = FmtText::from(&self.reasoning_skin, content, Some(80));
        let formatted_string = formatted.to_string();

        // Write each line through our write_line method
        for line in formatted_string.lines() {
            self.write_line_noclear(line);
        }

        self.write_line("");
    }

    /// Print a system/status message (indented)
    pub fn status(&self, message: &str) {
        self.write_line(&format!("  {}", message.dimmed()));
    }

    /// Print an info message (indented)
    pub fn info(&self, label: &str, value: &str) {
        self.write_line(&format!("  {} {}", label.bright_blue(), value));
    }

    /// Print a success message (indented)
    pub fn success(&self, message: &str) {
        self.write_line(&format!("  {} {}", "✓".bright_green(), message));
    }

    /// Print an error message (indented)
    pub fn error(&self, message: &str) {
        self.write_line(&format!("  {} {}", "✗".bright_red(), message));
    }

    /// Print a warning message (indented)
    pub fn warning(&self, message: &str) {
        self.write_line(&format!("  {} {}", "⚠".yellow(), message));
    }

    /// Print a section header
    pub fn section(&self, title: &str) {
        self.write_line("");
        self.write_line(&title.bright_cyan().bold().to_string());
        self.write_line(&"─".repeat(40).dimmed().to_string());
    }

    /// Print a list item (already indented)
    pub fn list_item(&self, item: &str) {
        self.write_line(&format!("    • {}", item));
    }

    /// Print a tool call
    pub fn tool_call(&self, tool_name: &str, args: &str) {
        self.write_line_noclear(&format!(
            "  {} Using tool: {}",
            ">>".bright_blue(),
            tool_name.bright_yellow()
        ));
        if !args.is_empty() {
            // Indent each line of the args for proper alignment
            for (i, line) in args.lines().enumerate() {
                if i == 0 {
                    self.write_line_noclear(&format!("     Args: {}", line).dimmed().to_string());
                } else {
                    self.write_line_noclear(&format!("           {}", line).dimmed().to_string());
                }
            }
        }
        self.flush();
    }

    /// Print a tool result
    pub fn tool_result(&self, result: &str) {
        // Handle multi-line results with proper indentation
        let lines: Vec<&str> = result.lines().collect();
        if lines.len() == 1 {
            self.write_line_noclear(&format!(
                "  {} Tool result: {}",
                "=>".bright_green(),
                result.dimmed()
            ));
        } else {
            self.write_line_noclear(&format!("  {} Tool result:", "=>".bright_green()));
            for line in lines {
                self.write_line_noclear(&format!("     {}", line.dimmed()));
            }
        }
        self.flush();
    }

    /// Print a "working on it" status message
    /// For actual progress bars, use indicatif directly
    #[allow(dead_code)]
    pub fn working(&self, label: &str) {
        self.write_line(&format!("  {} {}...", "[...]".dimmed(), label));
    }

    /// Print a key-value pair (indented)
    pub fn kv(&self, key: &str, value: &str) {
        self.write_line(&format!("  {} {}", format!("{}:", key).dimmed(), value));
    }

    /// Print a raw line (no formatting)
    pub fn print(&self, content: &str) {
        self.write_line(content);
    }

    /// Print a prompt for user input
    #[allow(dead_code)]
    pub fn prompt(&self, prompt: &str) {
        print!("  {} ", prompt.bright_cyan());
        use std::io::{self, Write};
        io::stdout().flush().unwrap();
    }

    /// Print markdown content (not from agent)
    #[allow(dead_code)]
    pub fn markdown(&self, content: &str) {
        // Use termimad to format the markdown content
        use termimad::FmtText;
        let formatted = FmtText::from(&self.skin, content, Some(80));
        let formatted_string = formatted.to_string();

        // Write each line through our write_line method
        for line in formatted_string.lines() {
            self.write_line_noclear(line);
        }
        self.flush();
    }

    /// Print a table-like header
    #[allow(dead_code)]
    pub fn table_header(&self, columns: &[&str]) {
        let header = columns.join(" | ");
        self.write_line_noclear(&format!("  {}", header.bright_white().bold()));
        self.write_line(&format!("  {}", "─".repeat(header.len()).dimmed()));
    }

    /// Print a table row
    #[allow(dead_code)]
    pub fn table_row(&self, cells: &[&str]) {
        let row = cells.join(" | ");
        self.write_line(&format!("  {}", row));
    }
}

impl Default for Output {
    fn default() -> Self {
        Self::new()
    }
}

/// Format agent state for display
#[allow(dead_code)]
pub fn format_agent_state(state: &pattern_core::agent::AgentState) -> String {
    use pattern_core::agent::AgentState;

    match state {
        AgentState::Ready => "Ready".bright_green().to_string(),
        AgentState::Processing { active_batches } => {
            if active_batches.is_empty() {
                "Processing".bright_yellow().to_string()
            } else {
                format!("Processing ({} batches)", active_batches.len())
                    .bright_yellow()
                    .to_string()
            }
        }
        AgentState::Cooldown { until } => format!("Cooldown until {}", until.format("%H:%M:%S"))
            .yellow()
            .to_string(),
        AgentState::Suspended => "Suspended".bright_red().to_string(),
        AgentState::Error { kind, message } => format!("Error ({:?}): {}", kind, message)
            .red()
            .bold()
            .to_string(),
    }
}

/// Format a timestamp as relative time
pub fn format_relative_time(time: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(time);

    if duration.num_seconds() < 60 {
        format!("{} seconds ago", duration.num_seconds())
            .dimmed()
            .to_string()
    } else if duration.num_minutes() < 60 {
        format!("{} minutes ago", duration.num_minutes())
            .dimmed()
            .to_string()
    } else if duration.num_hours() < 24 {
        format!("{} hours ago", duration.num_hours())
            .dimmed()
            .to_string()
    } else if duration.num_days() < 30 {
        format!("{} days ago", duration.num_days())
            .dimmed()
            .to_string()
    } else {
        time.format("%Y-%m-%d").to_string().dimmed().to_string()
    }
}
