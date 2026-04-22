//! Status bar widget showing persona, agent count, context usage, and
//! connection state.
//!
//! Renders a single-line bar at the bottom of the TUI with styled segments:
//! `@persona | N agents | Xk ctx | ● connected`

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::layout::PanelVisibility;

/// How long status bar notifications persist.
const NOTIFICATION_TTL: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Status bar state
// ---------------------------------------------------------------------------

/// Data backing the status bar display. Updated periodically from daemon
/// status polls and step replies.
pub struct StatusBarState {
    /// Name of the fronting persona (displayed with `@` prefix).
    pub persona_name: String,
    /// Number of active agents.
    pub agent_count: usize,
    /// Current context token usage, if known.
    pub context_tokens: Option<u64>,
    /// Whether the TUI is connected to the daemon.
    pub connected: bool,
    /// Whether selection mode is currently active (AC4.8).
    pub selection_active: bool,
    /// Temporary notification message (auto-dismisses after TTL).
    pub notification: Option<String>,
    /// When the notification was created (for TTL expiry).
    pub notification_created_at: Option<Instant>,
}

impl Default for StatusBarState {
    fn default() -> Self {
        Self {
            persona_name: "unknown".into(),
            agent_count: 0,
            context_tokens: None,
            connected: false,
            selection_active: false,
            notification: None,
            notification_created_at: None,
        }
    }
}

impl StatusBarState {
    /// Set a temporary notification message that auto-dismisses after TTL.
    pub fn set_notification(&mut self, message: String) {
        self.notification = Some(message);
        self.notification_created_at = Some(Instant::now());
    }

    /// Remove expired notifications based on TTL.
    pub fn tick_notification(&mut self) {
        if let Some(created_at) = self.notification_created_at {
            if created_at.elapsed() >= NOTIFICATION_TTL {
                self.notification = None;
                self.notification_created_at = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Token formatting
// ---------------------------------------------------------------------------

/// Format a token count for compact display.
///
/// - Values below 1000 are shown as-is (e.g. `"450"`).
/// - Values in the thousands are shown as `"Nk"` (e.g. `45000 -> "45k"`).
/// - Values in the millions are shown as `"N.NM"` (e.g. `1234567 -> "1.2M"`).
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let millions = n as f64 / 1_000_000.0;
        format!("{:.1}M", millions)
    } else if n >= 1_000 {
        let thousands = n / 1_000;
        format!("{thousands}k")
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------------------
// Status bar widget
// ---------------------------------------------------------------------------

/// A single-line status bar widget.
pub struct StatusBar<'a> {
    /// The state to render.
    state: &'a StatusBarState,
    /// Current panel visibility, for the panel indicator segment.
    panel_visibility: PanelVisibility,
}

impl<'a> StatusBar<'a> {
    /// Create a new status bar widget.
    pub fn new(state: &'a StatusBarState, panel_visibility: PanelVisibility) -> Self {
        Self {
            state,
            panel_visibility,
        }
    }
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let bar_bg = Color::Black;

        // Fill entire bar width with background.
        for x in area.x..area.x + area.width {
            buf[(x, area.y)].set_style(Style::default().bg(bar_bg));
        }

        let mut spans = Vec::new();

        // Persona name segment.
        spans.push(Span::styled(
            format!(" @{}", self.state.persona_name),
            Style::default()
                .fg(Color::White)
                .bg(bar_bg)
                .add_modifier(Modifier::BOLD),
        ));

        // Separator.
        spans.push(Span::styled(
            " │ ",
            Style::default().fg(Color::DarkGray).bg(bar_bg),
        ));

        // Agent count segment.
        let agent_text = if self.state.agent_count == 1 {
            "1 agent".to_string()
        } else {
            format!("{} agents", self.state.agent_count)
        };
        spans.push(Span::styled(
            agent_text,
            Style::default().fg(Color::DarkGray).bg(bar_bg),
        ));

        // Context tokens segment (only if known).
        if let Some(tokens) = self.state.context_tokens {
            spans.push(Span::styled(
                " │ ",
                Style::default().fg(Color::DarkGray).bg(bar_bg),
            ));
            spans.push(Span::styled(
                format!("{} ctx", format_tokens(tokens)),
                Style::default().fg(Color::DarkGray).bg(bar_bg),
            ));
        }

        // Separator before connection indicator.
        spans.push(Span::styled(
            " │ ",
            Style::default().fg(Color::DarkGray).bg(bar_bg),
        ));

        // Connection indicator.
        if self.state.connected {
            spans.push(Span::styled(
                "●",
                Style::default().fg(Color::Green).bg(bar_bg),
            ));
            spans.push(Span::styled(
                " connected",
                Style::default().fg(Color::DarkGray).bg(bar_bg),
            ));
        } else {
            spans.push(Span::styled(
                "●",
                Style::default().fg(Color::Red).bg(bar_bg),
            ));
            spans.push(Span::styled(
                " disconnected",
                Style::default().fg(Color::DarkGray).bg(bar_bg),
            ));
        }

        // Panel state indicator (only when panel is not hidden).
        if self.panel_visibility != PanelVisibility::Hidden {
            spans.push(Span::styled(
                " │ ",
                Style::default().fg(Color::DarkGray).bg(bar_bg),
            ));
            let panel_label = match self.panel_visibility {
                PanelVisibility::Visible => "panel: visible",
                PanelVisibility::Expanded => "panel: expanded",
                PanelVisibility::Hidden => unreachable!(),
            };
            spans.push(Span::styled(
                panel_label,
                Style::default().fg(Color::DarkGray).bg(bar_bg),
            ));
        }

        // Selection mode indicator.
        if self.state.selection_active {
            spans.push(Span::styled(
                " │ ",
                Style::default().fg(Color::DarkGray).bg(bar_bg),
            ));
            spans.push(Span::styled(
                "[SELECT]",
                Style::default()
                    .fg(Color::Yellow)
                    .bg(bar_bg)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        // Notification message (appended to status bar content).
        if let Some(ref notif) = self.state.notification {
            spans.push(Span::styled(
                " │ ",
                Style::default().fg(Color::DarkGray).bg(bar_bg),
            ));
            spans.push(Span::styled(
                format!("{notif}"),
                Style::default()
                    .fg(Color::Cyan)
                    .bg(bar_bg)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        let line = Line::from(spans);
        buf.set_line(area.x, area.y, &line, area.width);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_utils::buffer_to_string;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Helper: render the StatusBar into a TestBackend and return the buffer
    /// content as a string.
    fn render_status_bar(state: &StatusBarState, panel_vis: PanelVisibility, width: u16) -> String {
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let widget = StatusBar::new(state, panel_vis);
                widget.render(f.area(), f.buffer_mut());
            })
            .unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    // -----------------------------------------------------------------------
    // Unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn token_formatting() {
        assert_eq!(format_tokens(450), "450");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1000), "1k");
        assert_eq!(format_tokens(45000), "45k");
        assert_eq!(format_tokens(999999), "999k");
        assert_eq!(format_tokens(1000000), "1.0M");
        assert_eq!(format_tokens(1234567), "1.2M");
        assert_eq!(format_tokens(10500000), "10.5M");
    }

    // -----------------------------------------------------------------------
    // Snapshot tests
    // -----------------------------------------------------------------------

    #[test]
    fn status_bar_connected() {
        let state = StatusBarState {
            persona_name: "supervisor".into(),
            agent_count: 3,
            context_tokens: Some(45000),
            connected: true,
            selection_active: false,
            notification: None,
            notification_created_at: None,
        };
        let output = render_status_bar(&state, PanelVisibility::Hidden, 60);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn status_bar_disconnected() {
        let state = StatusBarState {
            persona_name: "supervisor".into(),
            agent_count: 0,
            context_tokens: None,
            connected: false,
            selection_active: false,
            notification: None,
            notification_created_at: None,
        };
        let output = render_status_bar(&state, PanelVisibility::Hidden, 60);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn status_bar_with_panel_visible() {
        let state = StatusBarState {
            persona_name: "supervisor".into(),
            agent_count: 2,
            context_tokens: Some(8500),
            connected: true,
            selection_active: false,
            notification: None,
            notification_created_at: None,
        };
        let output = render_status_bar(&state, PanelVisibility::Visible, 70);
        insta::assert_snapshot!(output);
    }
}
