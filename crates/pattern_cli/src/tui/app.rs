//! Core TUI application struct and async event loop.
//!
//! [`App`] multiplexes terminal input (key/mouse/resize), daemon subscription
//! events ([`TaggedTurnEvent`]), and a periodic UI refresh tick using
//! [`tokio::select!`]. The terminal is rendered each iteration via ratatui.

use std::time::Duration;

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use smol_str::SmolStr;
use tokio::time;

use pattern_core::traits::turn_sink::DisplayKind;
use pattern_server::client::DaemonClient;
use pattern_server::protocol::{TaggedTurnEvent, WireTurnEvent};

use super::autocomplete::{AutocompleteState, AutocompleteWidget, CommandSource, CompletionSource};
use super::commands::lookup_command;
use super::conversation::{ConversationState, ConversationView};
use super::input::{InputAction, InputHandler};
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
    /// Keystrokes go to the input area.
    Input,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// Top-level TUI application state.
pub struct App {
    /// Conversation rendering state (batches, scroll, focus).
    conversation: ConversationState,
    /// Input handler wrapping TextArea with history and submit semantics.
    input: InputHandler,
    /// Autocomplete popup state.
    autocomplete: AutocompleteState,
    /// Command completion source.
    command_source: CommandSource,
    /// Whether the event loop should exit.
    should_quit: bool,
    /// Which panel has keyboard focus.
    focus: Focus,
    /// Connection to the daemon, if available.
    client: Option<DaemonClient>,
    /// The agent currently receiving messages.
    current_agent: SmolStr,
    /// Whether we are connected to the daemon.
    connected: bool,
    /// Height of the conversation viewport from the last rendered frame.
    /// Used by key handlers so scroll calculations use the real terminal size.
    /// Defaults to 24 until the first frame is drawn.
    last_viewport_height: u16,
    /// Channel for receiving results from spawned async tasks (command results,
    /// send errors). Spawned tasks hold a clone of the sender; the event loop
    /// polls the receiver.
    result_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Available agents discovered during InitSession. Used by /front to
    /// validate the requested agent name before switching.
    available_agents: Vec<SmolStr>,
}

impl App {
    /// Create a new application with the given default agent_id.
    ///
    /// The `agent_id` is the resolved persona identifier (e.g.
    /// `"pattern-default"`), used for routing messages and displayed in
    /// the status bar.
    pub fn new(agent_id: SmolStr) -> Self {
        // Create the result channel. The sender lives on the struct so
        // spawned tasks can clone it; the receiver is kept in `run()`.
        let (result_tx, _result_rx_placeholder) = tokio::sync::mpsc::unbounded_channel();
        Self {
            conversation: ConversationState {
                batches: Vec::new(),
                scroll_offset: 0,
                auto_scroll: true,
                focused_section: None,
                click_targets: Vec::new(),
            },
            input: InputHandler::new(),
            autocomplete: AutocompleteState::new(),
            command_source: CommandSource,
            should_quit: false,
            focus: Focus::Input,
            client: None,
            current_agent: agent_id,
            connected: false,
            last_viewport_height: 24,
            result_tx,
            available_agents: Vec::new(),
        }
    }

    /// Set the list of available agents from the InitSession response.
    ///
    /// Called by the TUI startup after a successful `InitSession` so that
    /// `/front` can validate agent names against this list.
    pub fn set_available_agents(&mut self, agents: Vec<SmolStr>) {
        self.available_agents = agents;
    }

    /// Run the async event loop until the user quits.
    ///
    /// `event_rx` is the daemon subscription channel. `None` means offline
    /// mode (no daemon connected). `client` is the daemon RPC client for
    /// sending messages and commands.
    pub async fn run(
        &mut self,
        terminal: &mut Terminal<ratatui::prelude::CrosstermBackend<std::io::Stdout>>,
        mut event_rx: Option<DaemonEventReceiver>,
        client: Option<DaemonClient>,
    ) -> miette::Result<()> {
        use miette::IntoDiagnostic;

        self.client = client;
        self.connected = event_rx.is_some();

        // Replace the placeholder channel with one whose receiver we own
        // here in the run loop. Spawned tasks clone `self.result_tx`.
        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        self.result_tx = result_tx;

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
                            tracing::trace!("daemon event received: batch={}", tagged_event.batch_id);
                            self.handle_daemon_event(tagged_event);
                        }
                        Ok(None) => {
                            tracing::warn!("daemon subscription channel closed (Ok(None))");
                            self.connected = false;
                            event_rx = None;
                        }
                        Err(e) => {
                            tracing::warn!("daemon subscription recv error: {e:?}");
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
                // Branch 4: results from spawned async tasks (command results,
                // send errors). Pushes the formatted message into the conversation
                // so results are visible to the user rather than only logged.
                Some(msg) = result_rx.recv() => {
                    self.push_system_message(msg);
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
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Resize(_, _) => {
                // Invalidate all cached heights — the width may have changed.
                for batch in &mut self.conversation.batches {
                    for section in &mut batch.sections {
                        section.cached_height = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// Handle a mouse event: left-click toggles collapsible sections.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let click_row = mouse.row;

            // Find a click target whose y position matches the clicked row.
            if let Some(&(batch_idx, section_idx, _y)) = self
                .conversation
                .click_targets
                .iter()
                .find(|&&(_, _, y)| y == click_row)
            {
                // Toggle the section's collapsed state.
                if let Some(batch) = self.conversation.batches.get_mut(batch_idx)
                    && let Some(section) = batch.sections.get_mut(section_idx)
                {
                    section.collapsed = !section.collapsed;
                    section.cached_height = None;
                }
            }
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
                        // Use the height from the last rendered frame so that
                        // scroll boundary calculations are correct on any terminal size.
                        apply_action(action, &mut self.conversation, self.last_viewport_height);
                    }
                }
            }
            Focus::Input => {
                // When autocomplete is visible, intercept navigation keys.
                if self.autocomplete.is_visible() {
                    match key.code {
                        KeyCode::Tab | KeyCode::Down => {
                            self.autocomplete.next();
                            return;
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            self.autocomplete.prev();
                            return;
                        }
                        KeyCode::Enter => {
                            // Accept the selected completion.
                            if let Some(value) = self.autocomplete.accept() {
                                let replacement = format!("/{value} ");
                                self.input.set_text(&replacement);
                            }
                            self.autocomplete.hide();
                            return;
                        }
                        KeyCode::Esc => {
                            self.autocomplete.hide();
                            return;
                        }
                        _ => {
                            // Fall through to normal input handling, then
                            // update autocomplete below.
                        }
                    }
                }

                // Tab switches focus to conversation when autocomplete is hidden.
                if key.code == KeyCode::Tab && !self.autocomplete.is_visible() {
                    self.focus = Focus::Conversation;
                    return;
                }

                // Route to input handler.
                let action = self.input.handle_key(key);
                self.handle_input_action(action);
            }
        }
    }

    /// Process an [`InputAction`] returned by the input handler.
    fn handle_input_action(&mut self, action: InputAction) {
        match action {
            InputAction::Submit(parts) => {
                // Extract display text from parts.
                let user_text = text_from_parts(&parts);

                // Add user message to conversation.
                let batch_id: SmolStr = format!("user-{}", self.conversation.batches.len()).into();
                let batch = RenderBatch::new(batch_id.clone(), Some(user_text));
                self.conversation.batches.push(batch);

                // Send to daemon if connected.
                if let Some(client) = &self.client {
                    let agent_id = self.current_agent.clone();
                    let client = client.clone();
                    let bid = batch_id;
                    let result_tx = self.result_tx.clone();
                    tokio::spawn(async move {
                        tracing::debug!("sending message batch={bid} agent={agent_id}");
                        if let Err(e) = client.send_message(bid.clone(), agent_id, parts).await {
                            tracing::error!("send_message failed batch={bid}: {e:?}");
                            let _ = result_tx.send(format!("send failed: {e}"));
                        }
                    });
                }

                // Hide autocomplete on submit.
                self.autocomplete.hide();
            }
            InputAction::SlashCommand { name, args } => {
                self.dispatch_command(&name, &args);
                self.autocomplete.hide();
            }
            InputAction::Changed => {
                self.update_autocomplete();
            }
            InputAction::None => {}
        }
    }

    /// Dispatch a slash command by name.
    fn dispatch_command(&mut self, name: &str, args: &[String]) {
        match lookup_command(name) {
            Some(cmd) => {
                use super::commands::CommandTarget;
                match cmd.target {
                    CommandTarget::Local => self.dispatch_local_command(name, args),
                    CommandTarget::Runtime => self.dispatch_runtime_command(name, args),
                }
            }
            None => {
                // Check for plugin-namespaced command (contains ':').
                if name.contains(':') {
                    self.dispatch_namespaced_command(name, args);
                } else {
                    self.push_system_message(format!(
                        "unknown command: /{name}. Type / for available commands."
                    ));
                }
            }
        }
    }

    /// Handle a local command (no daemon interaction).
    fn dispatch_local_command(&mut self, name: &str, _args: &[String]) {
        match name {
            "clear" => {
                self.conversation.batches.clear();
            }
            "quit" => {
                self.should_quit = true;
            }
            "panel" => {
                // Phase 4 implements the panel. Placeholder acknowledgment.
                self.push_system_message("panel toggle not yet implemented.".into());
            }
            _ => {}
        }
    }

    /// Handle a runtime command (requires daemon).
    fn dispatch_runtime_command(&mut self, name: &str, args: &[String]) {
        match name {
            "front" => {
                if let Some(agent_name) = args.first() {
                    let agent_name = agent_name.trim_start_matches('@');
                    // Validate against the available agents list when populated.
                    // When the list is empty (offline or not yet received), allow
                    // the switch without validation.
                    if !self.available_agents.is_empty()
                        && !self
                            .available_agents
                            .iter()
                            .any(|a| a.as_str() == agent_name)
                    {
                        let list = self
                            .available_agents
                            .iter()
                            .map(|a| a.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.push_system_message(format!(
                            "unknown agent '{agent_name}'. available: {list}"
                        ));
                        return;
                    }
                    self.current_agent = SmolStr::from(agent_name);
                    self.push_system_message(format!("switched to agent: {agent_name}"));
                } else {
                    self.push_system_message(format!("current agent: {}", self.current_agent));
                }
            }
            "agents" | "status" | "context" => {
                if let Some(client) = &self.client {
                    let client = client.clone();
                    let cmd_name = name.to_string();
                    let args = args.to_vec();
                    let result_tx = self.result_tx.clone();
                    tokio::spawn(async move {
                        match client.run_command(cmd_name.clone(), args).await {
                            Ok(result) => {
                                let _ = result_tx.send(result.output);
                            }
                            Err(e) => {
                                let _ = result_tx.send(format!("/{cmd_name} failed: {e}"));
                            }
                        }
                    });
                    self.push_system_message(format!("/{name} sent to daemon..."));
                } else {
                    self.push_system_message("not connected to daemon.".into());
                }
            }
            "shutdown" => {
                if let Some(client) = &self.client {
                    let client = client.clone();
                    let result_tx = self.result_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = client.run_command("shutdown".into(), Vec::new()).await {
                            let _ = result_tx.send(format!("shutdown failed: {e}"));
                        }
                    });
                    self.push_system_message("shutdown requested.".into());
                    self.should_quit = true;
                } else {
                    self.push_system_message("not connected to daemon.".into());
                }
            }
            _ => {
                self.push_system_message(format!("unknown runtime command: /{name}"));
            }
        }
    }

    /// Forward a plugin-namespaced command to the daemon.
    fn dispatch_namespaced_command(&mut self, name: &str, args: &[String]) {
        if let Some(client) = &self.client {
            let client = client.clone();
            let cmd_name = name.to_string();
            let args = args.to_vec();
            let result_tx = self.result_tx.clone();
            tokio::spawn(async move {
                match client.run_command(cmd_name.clone(), args).await {
                    Ok(result) => {
                        let _ = result_tx.send(result.output);
                    }
                    Err(e) => {
                        let _ = result_tx.send(format!("/{cmd_name} failed: {e}"));
                    }
                }
            });
            self.push_system_message(format!("/{name} sent to daemon..."));
        } else {
            self.push_system_message("not connected to daemon.".into());
        }
    }

    /// Push a system message (note) into the conversation.
    ///
    /// `pub(crate)` so `run_tui()` in `main.rs` can surface session init
    /// errors as the first message before the event loop starts.
    pub(crate) fn push_system_message(&mut self, text: String) {
        let batch_id: SmolStr = format!("sys-{}", self.conversation.batches.len()).into();
        let mut batch = RenderBatch::new(batch_id, None);
        batch.push_event(&WireTurnEvent::Display {
            kind: DisplayKind::Note,
            text,
        });
        batch.streaming = false;
        self.conversation.batches.push(batch);
    }

    /// Update autocomplete based on current input text.
    fn update_autocomplete(&mut self) {
        let text = self.input.current_text();
        if let Some(without_slash) = text.strip_prefix('/')
            && !without_slash.contains(' ')
        {
            // Completing a command name. Empty pattern shows all commands.
            let candidates = self.command_source.candidates();
            self.autocomplete.update(without_slash, &candidates);
            return;
        }
        self.autocomplete.hide();
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
                // New batch — create with no user message (the TUI set
                // the user message when it sent, above).
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

        // Record the viewport height so key handlers can use the real size.
        self.last_viewport_height = layout.conversation.height;

        // Conversation area.
        ratatui::widgets::StatefulWidget::render(
            ConversationView,
            layout.conversation,
            frame.buffer_mut(),
            &mut self.conversation,
        );

        // Input area — render the real textarea.
        render_input_area(layout.input, frame.buffer_mut(), self.focus, &self.input);

        // Status bar.
        render_status_bar(
            layout.status_bar,
            frame.buffer_mut(),
            self.connected,
            &self.current_agent,
        );

        // Autocomplete popup (rendered on top of conversation).
        if self.autocomplete.is_visible() {
            let widget = AutocompleteWidget::new(&self.autocomplete);
            widget.render_above(layout.input, frame.buffer_mut());
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Extract plain text from content parts for display as a user message.
fn text_from_parts(parts: &[pattern_core::types::provider::ContentPart]) -> String {
    use pattern_core::types::provider::ContentPart;
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Render the input area with the InputHandler's textarea.
fn render_input_area(area: Rect, buf: &mut Buffer, focus: Focus, input: &InputHandler) {
    let prompt_colour = if focus == Focus::Input {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    // Render prompt glyph in first column.
    let prompt_line = Line::from(vec![Span::styled("❯ ", Style::default().fg(prompt_colour))]);
    buf.set_line(area.x, area.y, &prompt_line, area.width);

    // Render the textarea to the right of the prompt.
    if area.width > 2 {
        let textarea_area = Rect {
            x: area.x + 2,
            y: area.y,
            width: area.width.saturating_sub(2),
            height: area.height,
        };
        input.widget().render(textarea_area, buf);
    }
}

/// Render the status bar — subdued text on subtle background.
/// Uses ANSI `Black` bg which is typically slightly distinct from the terminal's
/// default background in most themes, giving a gentle visual separation.
fn render_status_bar(area: Rect, buf: &mut Buffer, connected: bool, current_agent: &str) {
    let bar_bg = Color::Black;
    // Fill entire bar width with background.
    for x in area.x..area.x + area.width {
        buf[(x, area.y)].set_style(Style::default().bg(bar_bg));
    }

    let (status_text, fg) = if connected {
        (format!(" pattern [{current_agent}]"), Color::DarkGray)
    } else {
        (
            format!(" pattern (offline) [{current_agent}]"),
            Color::DarkGray,
        )
    };

    let line = Line::from(vec![Span::styled(
        status_text,
        Style::default().fg(fg).bg(bar_bg),
    )]);
    buf.set_line(area.x, area.y, &line, area.width);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_utils::buffer_to_string;
    use pattern_core::types::turn::StopReason;
    use pattern_server::protocol::WireTurnEvent;
    use ratatui::backend::TestBackend;

    /// Render the app into a TestBackend and return the buffer as a string.
    fn render_app(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.render_frame(f)).unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn app_renders_empty_state() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        let output = render_app(&mut app, 60, 12);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn app_renders_with_one_batch() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));

        // Add a batch with a user message and text response.
        let mut batch = RenderBatch::new("batch-1".into(), Some("Hello agent".into()));
        batch.push_event(&WireTurnEvent::Text("The answer is **42**.".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        let output = render_app(&mut app, 60, 12);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn clear_command_empties_conversation() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));

        // Add some batches.
        app.conversation
            .batches
            .push(RenderBatch::new("b1".into(), Some("hello".into())));
        app.conversation
            .batches
            .push(RenderBatch::new("b2".into(), Some("world".into())));
        assert_eq!(app.conversation.batches.len(), 2);

        app.dispatch_command("clear", &[]);
        assert!(app.conversation.batches.is_empty());
    }

    #[test]
    fn quit_command_sets_should_quit() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        assert!(!app.should_quit);

        app.dispatch_command("quit", &[]);
        assert!(app.should_quit);
    }

    #[test]
    fn unknown_command_shows_error() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        assert!(app.conversation.batches.is_empty());

        app.dispatch_command("nonexistent", &[]);
        assert_eq!(app.conversation.batches.len(), 1);

        // The system message should contain the unknown command name.
        let batch = &app.conversation.batches[0];
        assert!(!batch.sections.is_empty());
        match &batch.sections[0].kind {
            super::super::model::SectionKind::Display { text, .. } => {
                assert!(
                    text.contains("unknown command: /nonexistent"),
                    "error message should mention the unknown command, got: {text}"
                );
            }
            other => panic!("expected Display section, got {other:?}"),
        }
    }

    #[test]
    fn submit_creates_batch_with_user_message() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        assert!(app.conversation.batches.is_empty());

        // Simulate submitting text.
        let parts = vec![pattern_core::types::provider::ContentPart::Text(
            "hello world".into(),
        )];
        app.handle_input_action(InputAction::Submit(parts));

        assert_eq!(app.conversation.batches.len(), 1);
        assert_eq!(
            app.conversation.batches[0].user_message.as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn front_command_updates_current_agent() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        assert_eq!(app.current_agent.as_str(), "pattern-default");

        app.dispatch_command("front", &["@supervisor".into()]);
        assert_eq!(app.current_agent.as_str(), "supervisor");

        // Should also push a system message confirming the switch.
        assert!(!app.conversation.batches.is_empty());
    }

    #[test]
    fn slash_command_from_input_dispatches() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        assert!(!app.should_quit);

        // Simulate receiving a SlashCommand action from the input handler.
        app.handle_input_action(InputAction::SlashCommand {
            name: "quit".into(),
            args: vec![],
        });
        assert!(app.should_quit);
    }
}
