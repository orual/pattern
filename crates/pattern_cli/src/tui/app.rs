//! Core TUI application struct and async event loop.
//!
//! [`App`] multiplexes terminal input (key/mouse/resize), daemon subscription
//! events ([`TaggedTurnEvent`]), and a periodic UI refresh tick using
//! [`tokio::select!`]. The terminal is rendered each iteration via ratatui.

use std::sync::Mutex;
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
use super::layout::{
    DEFAULT_PANEL_PCT, MAX_PANEL_PCT, MIN_PANEL_PCT, MIN_PANEL_WIDTH, PanelVisibility,
    compute_layout_with_panel,
};
use super::model::{RenderBatch, SectionKind};
use super::panel::{PanelContent, PanelState, SidePanel};
use super::scroll::{ConversationAction, apply_action, map_key_to_action};
use super::status_bar::{StatusBar, StatusBarState};
use super::toast::{ToastState, render_toasts};

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
// Selection mode
// ---------------------------------------------------------------------------

/// Selection mode state for mouse drag-to-copy (AC4.8).
///
/// When active (toggled via Ctrl+S), shows visual indicator and enables
/// explicit selection mode. Automatic drag-to-select always works.
#[derive(Debug, Default)]
struct SelectionState {
    /// Start position (column, row) of the selection.
    start: Option<(u16, u16)>,
    /// Current position during drag (column, row).
    current: Option<(u16, u16)>,
    /// Whether we're currently dragging (moved > threshold from start).
    is_dragging: bool,
    /// Whether explicit selection mode is active (toggled via Ctrl+S).
    active: bool,
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
    /// Number of active agents (from daemon status polls).
    agent_count: usize,
    /// Total context tokens from loaded history.
    context_tokens: u64,
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
    /// Mutable state for the side panel (notes, display content, thinking).
    panel_state: PanelState,
    /// Active toast notifications (visible when panel is hidden).
    toast_state: ToastState,
    /// Current panel visibility state (Hidden/Visible/Expanded).
    panel_visibility: PanelVisibility,
    /// Panel width as a percentage of terminal width (15..=50).
    panel_pct: u16,
    /// Selection mode state for mouse drag-to-copy.
    selection: SelectionState,
    /// Buffer snapshot from the last rendered frame. Used by selection mode
    /// to extract text at the coordinates the user dragged over.
    last_rendered_buffer: Option<Buffer>,
    /// System clipboard handle (arboard). Kept alive for the TUI session
    /// to avoid "clipboard dropped" errors on platforms where clipboard
    /// connections need to persist.
    clipboard: Option<Mutex<arboard::Clipboard>>,
    /// Status bar state.
    status_bar: StatusBarState,
    /// Terminal width from the last rendered frame. Used by keybindings that
    /// need to know whether the terminal is wide enough for a split-panel view.
    /// Defaults to 0 until the first frame is drawn.
    terminal_width: u16,
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
            agent_count: 0,
            context_tokens: 0,
            last_viewport_height: 24,
            result_tx,
            available_agents: Vec::new(),
            panel_state: PanelState::default(),
            toast_state: ToastState::default(),
            panel_visibility: PanelVisibility::Hidden,
            panel_pct: DEFAULT_PANEL_PCT,
            selection: SelectionState::default(),
            last_rendered_buffer: None,
            // Initialize clipboard if available. May fail on some platforms
            // (e.g., headless systems), so we store None in that case.
            clipboard: arboard::Clipboard::new().ok().map(Mutex::new),
            status_bar: StatusBarState::default(),
            terminal_width: 0,
        }
    }

    /// Set the list of available agents from the InitSession response.
    ///
    /// Called by the TUI startup after a successful `InitSession` so that
    /// `/front` can validate agent names against this list.
    pub fn set_available_agents(&mut self, agents: Vec<SmolStr>) {
        self.agent_count = agents.len();
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
        // Status polls are expensive (RPC round-trip); only poll every 5 seconds.
        // We count 100ms ticks and fire on every 50th (5000ms / 100ms = 50).
        let mut tick_count: u32 = 0;

        // Initial draw.
        let completed = terminal.draw(|f| self.render_frame(f)).into_diagnostic()?;
        self.last_rendered_buffer = Some(completed.buffer.clone());

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
                // Branch 3: periodic UI refresh tick (every 100ms).
                _ = tick.tick() => {
                    // Expire old toasts and status bar notifications.
                    self.toast_state.tick();
                    self.tick_notification();
                    // Poll daemon status every 5 seconds (every 50th tick).
                    tick_count = tick_count.wrapping_add(1);
                    if tick_count % 50 == 1 {
                        self.poll_daemon_status();
                    }
                }
                // Branch 4: results from spawned async tasks (command results,
                // send errors). Pushes the formatted message into the conversation
                // so results are visible to the user rather than only logged.
                Some(msg) = result_rx.recv() => {
                    // Handle special status update messages.
                    if let Some(status_str) = msg.strip_prefix("STATUS:") {
                        if let Ok(count) = status_str.parse::<usize>() {
                            self.agent_count = count;
                        }
                    } else {
                        self.push_system_message(msg);
                    }
                }
            }

            if self.should_quit {
                break;
            }

            let completed = terminal.draw(|f| self.render_frame(f)).into_diagnostic()?;
            self.last_rendered_buffer = Some(completed.buffer.clone());
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

    /// Handle a mouse event.
    ///
    /// In explicit selection mode (Ctrl+S): only drag-to-select works.
    /// In normal mode: drag-to-select works, clicks toggle collapsible sections.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        const DRAG_THRESHOLD: u16 = 3; // Pixels before click becomes drag

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection.start = Some((mouse.column, mouse.row));
                self.selection.current = Some((mouse.column, mouse.row));
                self.selection.is_dragging = false;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                tracing::debug!("Drag event at ({}, {})", mouse.column, mouse.row);
                if let Some(start) = self.selection.start {
                    self.selection.current = Some((mouse.column, mouse.row));
                    // Check if moved more than threshold to distinguish click from drag.
                    let dx = (mouse.column as i16 - start.0 as i16).abs();
                    let dy = (mouse.row as i16 - start.1 as i16).abs();
                    if dx + dy > DRAG_THRESHOLD as i16 {
                        tracing::debug!("Drag threshold exceeded, is_dragging=true");
                        self.selection.is_dragging = true;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(start) = self.selection.start {
                    let end = self.selection.current.unwrap_or(start);

                    if self.selection.is_dragging {
                        // This was a drag → copy selection to clipboard.
                        let text = self.extract_text_from_buffer(start, end);
                        if !text.is_empty() {
                            let result = if let Some(clipboard) = &self.clipboard {
                                match clipboard.lock() {
                                    Ok(mut guard) => {
                                        // Temporarily release lock by cloning the text
                                        // and doing the operation within a闭包.
                                        let text_clone = text.clone();
                                        guard.set_text(text_clone).map_err(|e| {
                                            format!("failed to set clipboard text: {e}")
                                        })
                                    }
                                    Err(e) => Err(format!("clipboard lock failed: {e}")),
                                }
                            } else {
                                Err("clipboard not available".to_string())
                            };

                            match result {
                                Ok(()) => {
                                    self.status_bar
                                        .set_notification(format!("copied {} chars", text.len()));
                                }
                                Err(e) => {
                                    self.status_bar
                                        .set_notification(format!("clipboard error: {e}"));
                                }
                            }
                        }
                    } else if !self.selection.active {
                        // This was a click in normal mode → toggle collapsible section.
                        // In explicit selection mode, clicks don't toggle sections.
                        let click_row = mouse.row;
                        if let Some(&(batch_idx, section_idx, _y)) = self
                            .conversation
                            .click_targets
                            .iter()
                            .find(|&&(_, _, y)| y == click_row)
                        {
                            if let Some(batch) = self.conversation.batches.get_mut(batch_idx)
                                && let Some(section) = batch.sections.get_mut(section_idx)
                            {
                                section.collapsed = !section.collapsed;
                                section.cached_height = None;
                            }
                        }
                    }
                }
                // Clear drag state but keep explicit mode if active.
                let was_active = self.selection.active;
                self.selection.start = None;
                self.selection.current = None;
                self.selection.is_dragging = false;
                self.selection.active = was_active;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                match self.focus {
                    Focus::Input => {
                        // Switch to conversation focus and apply scroll.
                        self.focus = Focus::Conversation;
                        let scroll_action = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                            ConversationAction::ScrollUp(3)
                        } else {
                            ConversationAction::ScrollDown(3)
                        };
                        apply_action(
                            scroll_action,
                            &mut self.conversation,
                            self.last_viewport_height,
                        );
                    }
                    Focus::Conversation => {
                        let scroll_action = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                            ConversationAction::ScrollUp(3)
                        } else {
                            ConversationAction::ScrollDown(3)
                        };
                        apply_action(
                            scroll_action,
                            &mut self.conversation,
                            self.last_viewport_height,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// Enter explicit selection mode (toggled via Ctrl+S).
    fn enter_selection_mode(&mut self) {
        self.selection.active = true;
        self.selection.start = None;
        self.selection.current = None;
        self.selection.is_dragging = false;
    }

    /// Exit explicit selection mode.
    fn exit_selection_mode(&mut self) {
        self.selection.active = false;
        self.selection.start = None;
        self.selection.current = None;
        self.selection.is_dragging = false;
    }

    /// Expire old status bar notifications.
    fn tick_notification(&mut self) {
        self.status_bar.tick_notification();
    }

    /// Poll the daemon for status updates (agent count, etc.).
    /// Called periodically from the UI tick handler.
    fn poll_daemon_status(&mut self) {
        let Some(client) = &self.client else {
            return;
        };

        // Spawn a task to poll status; we'll get the result via the result channel.
        let client_clone = client.clone();
        let result_tx = self.result_tx.clone();
        tokio::spawn(async move {
            match client_clone.get_status().await {
                Ok(status) => {
                    let _ = result_tx.send(format!("STATUS:{}", status.agent_count));
                }
                Err(e) => {
                    tracing::debug!("Failed to poll daemon status: {:?}", e);
                }
            }
        });
    }

    /// Extract text from the last rendered buffer between two screen positions.
    ///
    /// Reads characters from `last_rendered_buffer` line by line from start to
    /// end. Multi-line selections include a newline at the end of each full row.
    fn extract_text_from_buffer(&self, start: (u16, u16), end: (u16, u16)) -> String {
        let Some(buf) = &self.last_rendered_buffer else {
            return String::new();
        };

        let buf_area = buf.area;

        // Normalize so (r0, c0) is before (r1, c1) in reading order.
        let (r0, c0, r1, c1) = if (start.1, start.0) <= (end.1, end.0) {
            (start.1, start.0, end.1, end.0)
        } else {
            (end.1, end.0, start.1, start.0)
        };

        let mut result = String::new();
        for row in r0..=r1 {
            if row < buf_area.y || row >= buf_area.y + buf_area.height {
                continue;
            }
            let col_start = if row == r0 { c0 } else { buf_area.x };
            let col_end = if row == r1 {
                c1
            } else {
                buf_area.x + buf_area.width - 1
            };
            for col in col_start..=col_end {
                if col < buf_area.x || col >= buf_area.x + buf_area.width {
                    continue;
                }
                let cell = &buf[(col, row)];
                let sym = cell.symbol();
                result.push_str(sym);
            }
            // Add newline between rows in multi-line selections.
            if row < r1 {
                // Trim trailing whitespace from each row for cleaner copy.
                let trimmed = result.trim_end_matches(' ');
                let trim_len = trimmed.len();
                result.truncate(trim_len);
                result.push('\n');
            }
        }

        // Trim trailing whitespace from the final row.
        let trimmed = result.trim_end();
        trimmed.to_string()
    }

    /// Render visual highlighting for the current selection.
    fn render_selection_highlight(&self, start: (u16, u16), end: (u16, u16), buf: &mut Buffer) {
        let buf_area = buf.area;

        // Normalize coordinates.
        let (r0, c0, r1, c1) = if (start.1, start.0) <= (end.1, end.0) {
            (start.1, start.0, end.1, end.0)
        } else {
            (end.1, end.0, start.1, start.0)
        };

        tracing::debug!(
            "Rendering highlight: buf_area={:?}, selection=({},{} to {},{})",
            buf_area,
            r0,
            c0,
            r1,
            c1
        );
        let mut cells_highlighted = 0;

        // Render highlighted rectangle over selected area.
        for row in r0..=r1 {
            if row < buf_area.y || row >= buf_area.y + buf_area.height {
                tracing::debug!("Row {} outside buffer area", row);
                continue;
            }
            let col_start = if row == r0 { c0 } else { buf_area.x };
            let col_end = if row == r1 {
                c1
            } else {
                buf_area.x + buf_area.width - 1
            };

            for col in col_start..=col_end {
                if col < buf_area.x || col >= buf_area.x + buf_area.width {
                    continue;
                }
                // Use DarkGray background for selection highlight (consistent, visible).
                buf[(col, row)].set_style(ratatui::style::Style::default().bg(Color::DarkGray));
                cells_highlighted += 1;
            }
        }
        tracing::debug!("Highlighted {} cells", cells_highlighted);
    }

    /// Handle a key event based on current focus.
    fn handle_key(&mut self, key: KeyEvent) {
        // Global: Ctrl+C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // Global: Ctrl+S toggles explicit selection mode.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            if self.selection.active {
                self.exit_selection_mode();
            } else {
                self.enter_selection_mode();
            }
            return;
        }

        // In selection mode, Escape exits; any other non-mouse key also exits.
        if self.selection.active {
            self.exit_selection_mode();
            // Escape is consumed entirely; other keys fall through.
            if key.code == KeyCode::Esc {
                return;
            }
        }

        // Global: Ctrl+P cycles panel visibility.
        // On narrow terminals (too small for split view), skip Visible and only
        // toggle between Hidden and Expanded, since Expanded still works at any
        // width while Visible would be immediately auto-hidden anyway.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
            self.panel_visibility = if self.terminal_width < MIN_PANEL_WIDTH {
                match self.panel_visibility {
                    PanelVisibility::Hidden => PanelVisibility::Expanded,
                    PanelVisibility::Visible | PanelVisibility::Expanded => PanelVisibility::Hidden,
                }
            } else {
                self.panel_visibility.cycle()
            };
            return;
        }

        // Global: Alt+] increases panel width by 5%, clamped to MAX_PANEL_PCT.
        if key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Char(']') {
            self.panel_pct = (self.panel_pct + 5).min(MAX_PANEL_PCT);
            return;
        }

        // Global: Alt+[ decreases panel width by 5%, clamped to MIN_PANEL_PCT.
        if key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Char('[') {
            self.panel_pct = self.panel_pct.saturating_sub(5).max(MIN_PANEL_PCT);
            return;
        }

        match self.focus {
            Focus::Conversation => {
                match key.code {
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Char('p') => {
                        // If a thinking section is focused, expand it into the panel.
                        if let Some((batch_idx, section_idx)) = self.conversation.focused_section
                            && let Some(batch) = self.conversation.batches.get(batch_idx)
                            && let Some(section) = batch.sections.get(section_idx)
                            && let SectionKind::Thinking(content) = &section.kind
                        {
                            self.panel_state.expanded_thinking = Some(content.clone());
                            self.panel_state.content = PanelContent::Thinking;
                            // Make the panel visible if it is hidden.
                            if self.panel_visibility == PanelVisibility::Hidden {
                                self.panel_visibility = PanelVisibility::Visible;
                            }
                        }
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
                self.panel_visibility = self.panel_visibility.cycle();
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

    /// Load historical batches into the conversation.
    ///
    /// Called during TUI startup to populate the conversation with recent
    /// message history from the daemon.
    pub(crate) fn load_history(&mut self, history: Vec<pattern_server::protocol::HistoricalBatch>) {
        let mut total_tokens = 0;
        for batch in history {
            total_tokens += batch.tokens;
            let mut render_batch = RenderBatch::new(batch.batch_id.clone(), batch.user_message);
            for event in &batch.events {
                render_batch.push_event(event);
            }
            render_batch.streaming = false;
            self.conversation.batches.push(render_batch);
        }
        self.context_tokens = total_tokens;
        // Enable auto-scroll so history loads at the bottom.
        self.conversation.auto_scroll = true;
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
    ///
    /// `Display` events are routed to the panel (when visible) or toast
    /// popups (when hidden) instead of the conversation. All other events
    /// are pushed into the conversation batch as before.
    fn handle_daemon_event(&mut self, tagged: TaggedTurnEvent) {
        // Route Display events to panel/toast instead of the conversation batch.
        if let WireTurnEvent::Display { kind, ref text } = tagged.event {
            if self.panel_visibility == PanelVisibility::Hidden {
                match kind {
                    DisplayKind::Chunk => self.toast_state.push_chunk(text),
                    DisplayKind::Final => self.toast_state.push_final(text.clone()),
                    DisplayKind::Note => self.toast_state.push(text.clone()),
                }
            } else {
                match kind {
                    DisplayKind::Chunk => self.panel_state.push_chunk(text),
                    DisplayKind::Final => self.panel_state.set_final(text.clone()),
                    DisplayKind::Note => self.panel_state.push_note(text.clone()),
                }
            }
            return;
        }

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
        let layout = compute_layout_with_panel(frame.area(), self.panel_visibility, self.panel_pct);

        // Record terminal dimensions so key handlers can use the real size.
        self.last_viewport_height = layout.conversation.map(|r| r.height).unwrap_or(0);
        self.terminal_width = frame.area().width;

        // If auto-hide kicked in (terminal became too narrow for split view),
        // update stored state so the next Ctrl+P cycle starts from the correct
        // position rather than a phantom Visible state.
        if layout.panel_visibility != self.panel_visibility {
            self.panel_visibility = layout.panel_visibility;
        }

        // Conversation area (only render when present — None in Expanded mode).
        if let Some(conv_rect) = layout.conversation {
            ratatui::widgets::StatefulWidget::render(
                ConversationView,
                conv_rect,
                frame.buffer_mut(),
                &mut self.conversation,
            );
        }

        // Side panel (when visible or expanded).
        if let Some(panel_rect) = layout.panel {
            ratatui::widgets::StatefulWidget::render(
                SidePanel,
                panel_rect,
                frame.buffer_mut(),
                &mut self.panel_state,
            );
        }

        // Input area — always full width, always rendered.
        if layout.input.width > 0 && layout.input.height > 0 {
            render_input_area(layout.input, frame.buffer_mut(), self.focus, &self.input);
        }

        // Status bar.
        self.status_bar.persona_name = self.current_agent.to_string();
        self.status_bar.agent_count = self.agent_count;
        self.status_bar.context_tokens = Some(self.context_tokens);
        self.status_bar.connected = self.connected;
        self.status_bar.selection_active = self.selection.active;
        StatusBar::new(&self.status_bar, self.panel_visibility)
            .render(layout.status_bar, frame.buffer_mut());

        // Toast overlays (on top of everything, when there are active toasts).
        if !self.toast_state.is_empty() {
            render_toasts(frame.area(), frame.buffer_mut(), &self.toast_state);
        }

        // Autocomplete popup (rendered on top of conversation).
        if self.autocomplete.is_visible() {
            let widget = AutocompleteWidget::new(&self.autocomplete);
            widget.render_above(layout.input, frame.buffer_mut());
        }

        // Selection highlighting (rendered on top of everything).
        if let Some(start) = self.selection.start
            && let Some(end) = self.selection.current
        {
            self.render_selection_highlight(start, end, frame.buffer_mut());
        } else {
            if self.selection.start.is_some() {}
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

// Status bar rendering has been extracted to `super::status_bar::StatusBar`.

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

    /// Build a [`TaggedTurnEvent`] with a default agent_id for test convenience.
    fn tagged(batch_id: &str, event: WireTurnEvent) -> TaggedTurnEvent {
        TaggedTurnEvent {
            batch_id: batch_id.into(),
            agent_id: SmolStr::new_static("test-agent"),
            event,
        }
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

    // -------------------------------------------------------------------
    // Phase 4: panel, toast, and display routing tests
    // -------------------------------------------------------------------

    #[test]
    fn panel_command_cycles_visibility() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        assert_eq!(app.panel_visibility, PanelVisibility::Hidden);

        app.dispatch_command("panel", &[]);
        assert_eq!(app.panel_visibility, PanelVisibility::Visible);

        app.dispatch_command("panel", &[]);
        assert_eq!(app.panel_visibility, PanelVisibility::Expanded);

        app.dispatch_command("panel", &[]);
        assert_eq!(app.panel_visibility, PanelVisibility::Hidden);
    }

    #[test]
    fn ctrl_p_cycles_panel_wide_terminal() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        // Simulate a wide terminal so all three states are reachable.
        app.terminal_width = MIN_PANEL_WIDTH;
        assert_eq!(app.panel_visibility, PanelVisibility::Hidden);

        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        app.handle_key(ctrl_p);
        assert_eq!(app.panel_visibility, PanelVisibility::Visible);

        app.handle_key(ctrl_p);
        assert_eq!(app.panel_visibility, PanelVisibility::Expanded);

        app.handle_key(ctrl_p);
        assert_eq!(app.panel_visibility, PanelVisibility::Hidden);
    }

    #[test]
    fn ctrl_p_skips_visible_on_narrow_terminal() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        // terminal_width defaults to 0, which is < MIN_PANEL_WIDTH.
        assert_eq!(app.terminal_width, 0);
        assert_eq!(app.panel_visibility, PanelVisibility::Hidden);

        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        // On narrow terminals: Hidden -> Expanded (skip Visible).
        app.handle_key(ctrl_p);
        assert_eq!(app.panel_visibility, PanelVisibility::Expanded);

        // Expanded -> Hidden.
        app.handle_key(ctrl_p);
        assert_eq!(app.panel_visibility, PanelVisibility::Hidden);
    }

    #[test]
    fn alt_bracket_adjusts_panel_pct() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        assert_eq!(app.panel_pct, DEFAULT_PANEL_PCT); // 25

        let alt_right = KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT);
        app.handle_key(alt_right);
        assert_eq!(app.panel_pct, 30);

        let alt_left = KeyEvent::new(KeyCode::Char('['), KeyModifiers::ALT);
        app.handle_key(alt_left);
        assert_eq!(app.panel_pct, 25);

        // Clamp to max.
        for _ in 0..20 {
            app.handle_key(alt_right);
        }
        assert_eq!(app.panel_pct, MAX_PANEL_PCT);

        // Clamp to min.
        for _ in 0..20 {
            app.handle_key(alt_left);
        }
        assert_eq!(app.panel_pct, MIN_PANEL_PCT);
    }

    #[test]
    fn daemon_display_routes_to_toast_when_panel_hidden() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        app.panel_visibility = PanelVisibility::Hidden;

        // Simulate a daemon Display::Note event.
        app.handle_daemon_event(tagged(
            "batch-1",
            WireTurnEvent::Display {
                kind: DisplayKind::Note,
                text: "agent processing...".into(),
            },
        ));

        // Should go to toast, not conversation.
        assert!(
            app.conversation.batches.is_empty(),
            "Display event should not create a conversation batch"
        );
        assert_eq!(app.toast_state.toasts.len(), 1);
        assert_eq!(app.toast_state.toasts[0].text, "agent processing...");
    }

    #[test]
    fn daemon_display_routes_to_panel_when_visible() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        app.panel_visibility = PanelVisibility::Visible;

        // Note event.
        app.handle_daemon_event(tagged(
            "batch-1",
            WireTurnEvent::Display {
                kind: DisplayKind::Note,
                text: "a note".into(),
            },
        ));

        // Chunk event.
        app.handle_daemon_event(tagged(
            "batch-1",
            WireTurnEvent::Display {
                kind: DisplayKind::Chunk,
                text: "partial ".into(),
            },
        ));

        // Final event.
        app.handle_daemon_event(tagged(
            "batch-1",
            WireTurnEvent::Display {
                kind: DisplayKind::Final,
                text: "complete result".into(),
            },
        ));

        // Nothing in conversation or toasts.
        assert!(
            app.conversation.batches.is_empty(),
            "Display events should not create conversation batches"
        );
        assert!(
            app.toast_state.is_empty(),
            "Display events should not create toasts when panel is visible"
        );

        // Everything in panel state.
        assert_eq!(app.panel_state.notes.len(), 1);
        assert_eq!(app.panel_state.notes[0], "a note");
        assert_eq!(app.panel_state.display_content, "complete result");
    }

    #[test]
    fn daemon_non_display_events_still_go_to_conversation() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        app.panel_visibility = PanelVisibility::Visible;

        // Text event should go to conversation, not panel.
        app.handle_daemon_event(tagged(
            "batch-1",
            WireTurnEvent::Text("hello from agent".into()),
        ));

        assert_eq!(app.conversation.batches.len(), 1);
        assert_eq!(app.conversation.batches[0].sections.len(), 1);
    }

    #[test]
    fn push_system_message_still_goes_to_conversation() {
        // This is the critical test: push_system_message creates Display
        // events directly in a batch. They must NOT be rerouted.
        let mut app = App::new(SmolStr::new_static("pattern-default"));
        app.panel_visibility = PanelVisibility::Visible;

        app.push_system_message("a system note".into());

        // Must be in conversation, not panel or toast.
        assert_eq!(app.conversation.batches.len(), 1);
        assert!(app.toast_state.is_empty());
        assert!(app.panel_state.notes.is_empty());
    }

    #[test]
    fn thinking_expand_to_panel() {
        let mut app = App::new(SmolStr::new_static("pattern-default"));

        // Add a batch with thinking content.
        let mut batch = RenderBatch::new("batch-1".into(), Some("question".into()));
        batch.push_event(&WireTurnEvent::Thinking("deep reasoning here".into()));
        batch.push_event(&WireTurnEvent::Text("answer".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        // Focus on the thinking section (batch 0, section 0).
        app.conversation.focused_section = Some((0, 0));
        app.focus = Focus::Conversation;

        // Press 'p' to expand thinking into panel.
        let p_key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        app.handle_key(p_key);

        assert_eq!(
            app.panel_state.expanded_thinking.as_deref(),
            Some("deep reasoning here"),
        );
        assert_eq!(app.panel_state.content, PanelContent::Thinking);
        // Panel should be auto-shown.
        assert_eq!(app.panel_visibility, PanelVisibility::Visible);
    }

    // -------------------------------------------------------------------
    // Integration snapshot tests
    // -------------------------------------------------------------------

    #[test]
    fn full_app_with_panel_visible() {
        let mut app = App::new(SmolStr::new_static("supervisor"));
        app.connected = true;
        app.panel_visibility = PanelVisibility::Visible;
        app.panel_pct = 30;

        // Add a conversation batch.
        let mut batch = RenderBatch::new("batch-1".into(), Some("Hello agent".into()));
        batch.push_event(&WireTurnEvent::Text("The answer is **42**.".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        // Add some panel content.
        app.panel_state.push_note("agent started".into());
        app.panel_state.push_chunk("processing query...");

        // Use a wide terminal so the panel is visible (>= MIN_PANEL_WIDTH=100).
        let output = render_app(&mut app, 120, 16);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn full_app_with_panel_hidden() {
        let mut app = App::new(SmolStr::new_static("supervisor"));
        app.connected = true;
        app.panel_visibility = PanelVisibility::Hidden;

        // Add a conversation batch.
        let mut batch = RenderBatch::new("batch-1".into(), Some("Hello agent".into()));
        batch.push_event(&WireTurnEvent::Text("The answer is **42**.".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        // Verify zero chrome: conversation fills full width.
        let output = render_app(&mut app, 80, 12);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn thinking_expanded_in_panel() {
        let mut app = App::new(SmolStr::new_static("supervisor"));
        app.connected = true;
        app.panel_visibility = PanelVisibility::Visible;
        app.panel_pct = 30;

        // Add conversation with thinking.
        let mut batch = RenderBatch::new("batch-1".into(), Some("Analyze this".into()));
        batch.push_event(&WireTurnEvent::Thinking(
            "Let me consider the options carefully...\nOption A is good.\nOption B is better."
                .into(),
        ));
        batch.push_event(&WireTurnEvent::Text("I recommend option B.".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        // Set thinking content in panel.
        app.panel_state.expanded_thinking = Some(
            "Let me consider the options carefully...\nOption A is good.\nOption B is better."
                .into(),
        );
        app.panel_state.content = PanelContent::Thinking;

        let output = render_app(&mut app, 120, 16);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn display_note_as_toast_when_hidden() {
        let mut app = App::new(SmolStr::new_static("supervisor"));
        app.connected = true;
        app.panel_visibility = PanelVisibility::Hidden;

        // Add some conversation content so the display isn't empty.
        let mut batch = RenderBatch::new("batch-1".into(), Some("Hello".into()));
        batch.push_event(&WireTurnEvent::Text("World".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        // Simulate a Display::Note arriving from daemon.
        app.handle_daemon_event(tagged(
            "batch-2",
            WireTurnEvent::Display {
                kind: DisplayKind::Note,
                text: "agent processing query...".into(),
            },
        ));

        // The toast should be visible in the render.
        let output = render_app(&mut app, 80, 12);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn display_note_in_panel_when_visible() {
        let mut app = App::new(SmolStr::new_static("supervisor"));
        app.connected = true;
        app.panel_visibility = PanelVisibility::Visible;
        app.panel_pct = 30;

        // Add conversation content.
        let mut batch = RenderBatch::new("batch-1".into(), Some("Hello".into()));
        batch.push_event(&WireTurnEvent::Text("World".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        // Simulate Display::Note arriving from daemon — should go to panel.
        app.handle_daemon_event(tagged(
            "batch-2",
            WireTurnEvent::Display {
                kind: DisplayKind::Note,
                text: "agent processing query...".into(),
            },
        ));

        let output = render_app(&mut app, 120, 16);
        insta::assert_snapshot!(output);
    }
}
