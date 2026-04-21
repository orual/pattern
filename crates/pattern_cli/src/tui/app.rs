//! Core TUI application struct and async event loop.
//!
//! [`App`] multiplexes terminal input (key/mouse/resize), daemon subscription
//! events ([`TaggedTurnEvent`]), and a periodic UI refresh tick using
//! [`tokio::select!`]. The terminal is rendered each iteration via ratatui.

use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::Borders;
use ratatui_widgets::paragraph::Paragraph;
use tokio::time;

use pattern_server::protocol::TaggedTurnEvent;

use super::conversation::{ConversationState, ConversationView};
use super::layout::compute_layout;
use super::model::RenderBatch;
use super::scroll::{apply_action, map_key_to_action};

/// The receiver type for daemon subscription events.
pub type DaemonEventReceiver = irpc::channel::mpsc::Receiver<TaggedTurnEvent>;

// ---------------------------------------------------------------------------
// Focus
// ---------------------------------------------------------------------------

/// Which panel currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// Arrow keys scroll the conversation; Enter toggles sections.
    Conversation,
    /// Keystrokes go to the input area (Phase 3).
    Input,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// Top-level TUI application state.
pub struct App {
    /// Conversation rendering state (batches, scroll, focus).
    conversation: ConversationState,
    /// Whether the event loop should exit.
    should_quit: bool,
    /// Which panel has keyboard focus.
    focus: Focus,
    /// Whether we are connected to the daemon.
    connected: bool,
}

impl App {
    /// Create a new application.
    pub fn new() -> Self {
        Self {
            conversation: ConversationState {
                batches: Vec::new(),
                scroll_offset: 0,
                auto_scroll: true,
                focused_section: None,
            },
            should_quit: false,
            focus: Focus::Input,
            connected: false,
        }
    }

    /// Run the async event loop until the user quits.
    ///
    /// `event_rx` is the daemon subscription channel. `None` means offline
    /// mode (no daemon connected).
    pub async fn run(
        &mut self,
        terminal: &mut Terminal<ratatui::prelude::CrosstermBackend<std::io::Stdout>>,
        mut event_rx: Option<DaemonEventReceiver>,
    ) -> miette::Result<()> {
        use miette::IntoDiagnostic;

        self.connected = event_rx.is_some();

        let mut reader = EventStream::new();
        let mut tick = time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        // Initial draw.
        terminal.draw(|f| self.render_frame(f)).into_diagnostic()?;

        loop {
            tokio::select! {
                // Branch 1: terminal events (key, mouse, resize).
                maybe_event = reader.next() => {
                    match maybe_event {
                        Some(Ok(event)) => self.handle_terminal_event(event),
                        Some(Err(_)) => {
                            // Crossterm error reading events — bail.
                            self.should_quit = true;
                        }
                        None => {
                            // Stream ended.
                            self.should_quit = true;
                        }
                    }
                }
                // Branch 2: daemon subscription events.
                Some(recv_result) = async {
                    match event_rx.as_mut() {
                        Some(rx) => Some(rx.recv().await),
                        None => {
                            // No receiver — pend forever so this branch
                            // never fires.
                            std::future::pending::<Option<_>>().await
                        }
                    }
                } => {
                    match recv_result {
                        Ok(Some(tagged_event)) => {
                            self.handle_daemon_event(tagged_event);
                        }
                        Ok(None) => {
                            // Channel closed — daemon disconnected.
                            self.connected = false;
                            event_rx = None;
                        }
                        Err(_) => {
                            // Recv error — treat as disconnect.
                            self.connected = false;
                            event_rx = None;
                        }
                    }
                }
                // Branch 3: periodic UI refresh tick.
                _ = tick.tick() => {
                    // Just redraw — handles streaming cursor blink,
                    // toast expiry, status bar updates.
                }
            }

            if self.should_quit {
                break;
            }

            terminal.draw(|f| self.render_frame(f)).into_diagnostic()?;
        }

        Ok(())
    }

    /// Handle a terminal event (key press, mouse, resize).
    fn handle_terminal_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Resize(_, _) => {
                // Invalidate all cached heights — the width may have changed.
                for batch in &mut self.conversation.batches {
                    for section in &mut batch.sections {
                        section.cached_height = None;
                    }
                }
            }
            // Mouse events and others are ignored for now.
            _ => {}
        }
    }

    /// Handle a key event based on current focus.
    fn handle_key(&mut self, key: KeyEvent) {
        // Global: Ctrl+C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match self.focus {
            Focus::Conversation => {
                match key.code {
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Esc => {
                        // Switch back to input focus.
                        self.focus = Focus::Input;
                    }
                    _ => {
                        // Route to conversation scroll/expand actions.
                        let action = map_key_to_action(key, &self.conversation);
                        // Use a reasonable default viewport height; the actual
                        // height is set during draw, but for action computation
                        // we use the last known offset logic which is still valid.
                        apply_action(action, &mut self.conversation, 24);
                    }
                }
            }
            Focus::Input => {
                match key.code {
                    KeyCode::Esc => {
                        // Esc from input quits the app.
                        self.should_quit = true;
                    }
                    KeyCode::Tab => {
                        // Switch to conversation focus.
                        self.focus = Focus::Conversation;
                    }
                    _ => {
                        // Phase 3 handles actual text input. No-op for now.
                    }
                }
            }
        }
    }

    /// Handle a tagged turn event from the daemon.
    fn handle_daemon_event(&mut self, tagged: TaggedTurnEvent) {
        // Find existing batch by batch_id, or create a new one.
        let batch = match self
            .conversation
            .batches
            .iter_mut()
            .find(|b| b.batch_id == tagged.batch_id)
        {
            Some(b) => b,
            None => {
                // New batch — create with no user message (the TUI will set
                // the user message when it sends, in Phase 3).
                let new_batch = RenderBatch::new(tagged.batch_id.clone(), None);
                self.conversation.batches.push(new_batch);
                self.conversation.batches.last_mut().unwrap()
            }
        };
        batch.push_event(&tagged.event);
    }

    /// Draw the full TUI frame into the given frame.
    ///
    /// Extracted so that both `run()` (which owns the terminal) and tests
    /// (which use `terminal.draw()` directly) can share the rendering logic.
    fn render_frame(&mut self, frame: &mut ratatui::Frame<'_>) {
        let layout = compute_layout(frame.area());

        // Conversation area.
        ratatui::widgets::StatefulWidget::render(
            ConversationView,
            layout.conversation,
            frame.buffer_mut(),
            &mut self.conversation,
        );

        // Input area — placeholder until Phase 3.
        render_input_placeholder(layout.input, frame.buffer_mut(), self.focus);

        // Status bar.
        render_status_bar(layout.status_bar, frame.buffer_mut(), self.connected);
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Render a placeholder input area with a bordered block and grey hint text.
fn render_input_placeholder(area: Rect, buf: &mut Buffer, focus: Focus) {
    let border_style = if focus == Focus::Input {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title("input");

    let hint = Paragraph::new(Line::from(vec![Span::styled(
        "type here...",
        Style::default().fg(Color::DarkGray),
    )]))
    .block(block);

    hint.render(area, buf);
}

/// Render the status bar.
fn render_status_bar(area: Rect, buf: &mut Buffer, connected: bool) {
    let (text, style) = if connected {
        ("pattern", Style::default().fg(Color::Green))
    } else {
        ("pattern (offline)", Style::default().fg(Color::DarkGray))
    };

    let line = Line::from(vec![Span::styled(text, style)]);
    buf.set_line(area.x, area.y, &line, area.width);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::traits::turn_sink::TurnEvent;
    use pattern_core::types::turn::StopReason;
    use ratatui::backend::TestBackend;

    /// Convert a Buffer to a trimmed-right string representation.
    fn buffer_to_string(buf: &Buffer) -> String {
        let mut lines = Vec::new();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                line.push_str(cell.symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Render the app into a TestBackend and return the buffer as a string.
    fn render_app(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.render_frame(f)).unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn app_renders_empty_state() {
        let mut app = App::new();
        let output = render_app(&mut app, 60, 12);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn app_renders_with_one_batch() {
        let mut app = App::new();

        // Add a batch with a user message and text response.
        let mut batch = RenderBatch::new("batch-1".into(), Some("Hello agent".into()));
        batch.push_event(&TurnEvent::Text("The answer is **42**.".into()));
        batch.push_event(&TurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        let output = render_app(&mut app, 60, 12);
        insta::assert_snapshot!(output);
    }
}
