//! Core TUI application struct and async event loop.
//!
//! [`App`] multiplexes terminal input (key/mouse/resize), daemon subscription
//! events ([`TaggedTurnEvent`]), and a periodic UI refresh tick using
//! [`tokio::select!`]. The terminal is rendered each iteration via ratatui.

use std::collections::HashMap;
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
use pattern_core::types::ids::{new_id, new_snowflake_id};
use pattern_core::types::origin::{Author, MessageOrigin, Partner, Sphere};
use pattern_server::client::DaemonClient;
use pattern_server::protocol::{Recipient, TaggedTurnEvent, WireTurnEvent};

use super::autocomplete::{AutocompleteState, AutocompleteWidget, CompletionMode};
use super::commands::{
    CMD_AGENT, CMD_AGENTS, CMD_CANCEL, CMD_CLEAR, CMD_FLOAT, CMD_FRONT, CMD_PANE, CMD_PANEL,
    CMD_PROMOTE, CMD_QUIT, CMD_RELATE, CMD_SHUTDOWN, CMD_STATUS, CommandRegistry,
};
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
use super::zellij::detect::ZellijState;

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
    /// Command registry: built-in commands plus any daemon-provided extensions.
    command_registry: CommandRegistry,
    /// Whether the event loop should exit.
    should_quit: bool,
    /// Which panel has keyboard focus.
    focus: Focus,
    /// Connection to the daemon, if available.
    client: Option<DaemonClient>,
    /// Active fronting persona ids (driven by `SessionInfo.fronting_snapshot`
    /// at session init and `FrontingChanged` events thereafter).
    ///
    /// Phase 6 T8.
    fronting_active: Vec<SmolStr>,
    /// Optional fronting fallback persona id (same source as `fronting_active`).
    fronting_fallback: Option<SmolStr>,
    /// Persistent route lock set by `/front @<agent>`. When `Some`, every
    /// outbound message is sent as `Recipient::Direct(id)`, bypassing the
    /// fronting set. When `None` (default), sends use `Recipient::Auto` and
    /// the daemon's fronting resolver picks the destination.
    /// Cleared by bare `/front`.
    ///
    /// Phase 6 T8.
    route_lock: Option<SmolStr>,
    /// One-shot route override set by `/agent @<id>`. Used as
    /// `Recipient::Direct(id)` for the next outbound message and then
    /// cleared. Takes precedence over `route_lock` for that one send.
    ///
    /// Phase 6 T8.
    pending_one_shot: Option<SmolStr>,
    /// Cached constellation registry view used by the constellation panel.
    /// Populated on session init; refreshed on every
    /// `WireTurnEvent::ConstellationChanged` notification.
    ///
    /// Phase 6 T8.
    constellation_view: super::constellation_view::ConstellationView,
    /// Latest known routing rules from the daemon's fronting set. Updated
    /// on every `FrontingChanged` event; used by the constellation panel.
    fronting_rules: Vec<pattern_server::protocol::WireRoutingRule>,
    /// Stable identity for this TUI session. Minted once at startup and used
    /// to construct `Author::Partner` origins on outbound messages. A fresh
    /// id is minted per-process so that concurrent TUI sessions are
    /// distinguishable in the agent's message history.
    partner_id: SmolStr,
    /// Optional human-readable display name for this partner, sourced from
    /// daemon `SessionInfo.partner_display_name` after `InitSession`.
    partner_display_name: Option<String>,
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
    /// Persona-name aliases (alias → canonical_id) discovered during
    /// InitSession. Used by `/front` and `/agent` to accept either the
    /// canonical id or the persona's display `name` and resolve to the
    /// canonical form before storing or sending.
    agent_aliases: HashMap<SmolStr, SmolStr>,
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
    /// Zellij environment state detected at startup. Drives `/pane` and
    /// `/float` availability and the auto-session launch decision.
    zellij_state: ZellijState,
}

impl App {
    /// Create a new mount-scoped application.
    ///
    /// Phase 6 T8: the TUI is mount-scoped, not agent-scoped. Routing is
    /// driven by the daemon's fronting set; outbound messages default to
    /// `Recipient::Auto`. Use `/front @<id>` to lock to a single agent,
    /// or `/agent @<id>` for a one-shot direct override.
    pub fn new() -> Self {
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
            command_registry: CommandRegistry::new(),
            should_quit: false,
            focus: Focus::Input,
            client: None,
            fronting_active: Vec::new(),
            fronting_fallback: None,
            route_lock: None,
            pending_one_shot: None,
            constellation_view: super::constellation_view::ConstellationView::default(),
            fronting_rules: Vec::new(),
            // Mint a stable partner identity for this TUI process. The daemon no
            // longer generates partner IDs — each client owns its own. Using
            // `new_id()` (UUID-v4) guarantees this TUI session is distinguishable
            // from other concurrent sessions in the agent's message history.
            partner_id: new_id(),
            partner_display_name: None,
            connected: false,
            agent_count: 0,
            context_tokens: 0,
            last_viewport_height: 24,
            result_tx,
            available_agents: Vec::new(),
            agent_aliases: HashMap::new(),
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
            zellij_state: ZellijState::NotAvailable,
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

    /// Set the persona-name alias map from the InitSession response.
    /// Called after `set_available_agents`. Aliases let users address
    /// agents by their persona `name` field; resolution to canonical
    /// agent_id happens locally before any RPC.
    pub fn set_agent_aliases(&mut self, aliases: Vec<pattern_server::protocol::AgentAlias>) {
        self.agent_aliases = aliases
            .into_iter()
            .map(|a| (a.alias, a.canonical_id))
            .collect();
    }

    /// Resolve a user-supplied agent handle (canonical id or alias, with
    /// or without leading `@`) to its canonical agent id. Returns `None`
    /// if the handle matches neither.
    fn resolve_agent_handle(&self, handle: &str) -> Option<SmolStr> {
        let stripped = handle.trim_start_matches('@');
        if self.available_agents.iter().any(|a| a.as_str() == stripped) {
            return Some(SmolStr::from(stripped));
        }
        self.agent_aliases.get(stripped).cloned()
    }

    /// Format the available-agents list (canonical ids + aliases) for
    /// error messages. Used by `/front` and `/agent` when the user types
    /// an unknown handle.
    fn format_addressable_agents(&self) -> String {
        let mut parts: Vec<String> = self
            .available_agents
            .iter()
            .map(|a| a.to_string())
            .collect();
        for (alias, canonical) in &self.agent_aliases {
            parts.push(format!("{alias} → {canonical}"));
        }
        parts.join(", ")
    }

    /// Register plugin commands fetched from the daemon on session init.
    ///
    /// Each item is a `(name, description)` pair. Commands with names that
    /// already exist in the built-in registry are silently ignored — built-ins
    /// always take precedence.
    pub fn set_daemon_commands(&mut self, commands: Vec<(String, String)>) {
        self.command_registry.register_daemon_commands(commands);
    }

    /// Override the TUI's partner identity with the one provided by the daemon.
    ///
    /// Called from the startup path after a successful `InitSession`, using the
    /// `partner_id` from [`pattern_server::protocol::SessionInfo`]. This ensures
    /// the TUI uses the daemon's stable identity rather than the per-process
    /// self-minted one, so the agent's message history shows consistent
    /// `Author::Partner` attribution across reconnections.
    ///
    /// Phase 6 Task 8 will wire multi-fronting routing through this path.
    pub fn set_partner_id(&mut self, partner_id: SmolStr) {
        self.partner_id = partner_id;
    }

    /// Set the human-readable display name for this partner.
    ///
    /// Called from the startup path when `SessionInfo.partner_display_name` is
    /// non-empty after a successful `InitSession`. Used to populate
    /// `Author::Partner.display_name` on outbound messages so attribution in
    /// the agent's message history is human-readable.
    pub fn set_partner_display_name(&mut self, name: String) {
        self.partner_display_name = Some(name);
    }

    /// Phase 6 T8: seed the fronting state from `SessionInfo.fronting_snapshot`.
    ///
    /// Live updates after this come via `WireTurnEvent::FrontingChanged`
    /// events on the all-mount stream; this is the initial state at startup
    /// so the status bar renders correctly before the first event arrives.
    pub fn set_fronting_snapshot(&mut self, snapshot: pattern_server::protocol::FrontingSnapshot) {
        self.fronting_active = snapshot.active.into_iter().map(SmolStr::from).collect();
        self.fronting_fallback = snapshot.fallback.map(SmolStr::from);
        self.fronting_rules = snapshot.rules;
    }

    /// Phase 6 T8: kick off a background fetch of personas + groups from
    /// the daemon. Updates `constellation_view` when the response arrives.
    /// Called on session init and on every `ConstellationChanged` event.
    pub fn refresh_constellation_view(&self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let result_tx = self.result_tx.clone();
        // We'd write to constellation_view here, but the App is `&self` from
        // the spawned context. Instead, deliver the result through the existing
        // result_tx channel as a tagged variant the App's main loop applies.
        //
        // To keep the channel string-based for now, encode it as a JSON blob
        // and have the App parse it on receipt. This is a pragmatic stopgap
        // — a typed result channel would be cleaner but is out of scope here.
        tokio::spawn(async move {
            let personas = client.list_personas(None).await;
            let groups = client.list_groups(None).await;
            // Encode both into a single result message the main loop
            // recognises by prefix.
            let personas_json = match personas {
                Ok(r) if r.error.is_none() => serde_json::to_string(&r.personas).ok(),
                _ => None,
            };
            let groups_json = match groups {
                Ok(r) if r.error.is_none() => serde_json::to_string(&r.groups).ok(),
                _ => None,
            };
            if let (Some(p), Some(g)) = (personas_json, groups_json) {
                let payload = format!("__constellation_view\x1f{p}\x1f{g}");
                let _ = result_tx.send(payload);
            }
        });
    }

    /// Update the zellij environment state.
    ///
    /// Called from `run_chat()` after detecting the zellij state at startup.
    /// Drives `/pane` and `/float` availability.
    pub fn set_zellij_state(&mut self, state: ZellijState) {
        self.zellij_state = state;
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
                    } else if let Some(payload) = msg.strip_prefix("__constellation_view\x1f") {
                        // Phase 6 T8: ConstellationView refresh result.
                        // Body shape: "<personas-json>\x1f<groups-json>".
                        if let Some((p_json, g_json)) = payload.split_once('\x1f') {
                            if let (Ok(personas), Ok(groups)) = (
                                serde_json::from_str::<
                                    Vec<pattern_server::protocol::WirePersonaSummary>,
                                >(p_json),
                                serde_json::from_str::<
                                    Vec<pattern_server::protocol::WireGroupSummary>,
                                >(g_json),
                            ) {
                                self.constellation_view.personas = personas;
                                self.constellation_view.groups = groups;
                                self.constellation_view.loaded = true;
                            }
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
            Event::Paste(text) => {
                // Bracketed paste: insert the pasted text into the input
                // textarea. This preserves newlines instead of treating
                // each line as a separate Enter keypress.
                if self.focus == Focus::Input {
                    self.input.insert_text(&text);
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
                            let char_count = text.len();
                            let result = if let Some(clipboard) = &self.clipboard {
                                match clipboard.lock() {
                                    Ok(mut guard) => guard
                                        .set_text(text)
                                        .map_err(|e| format!("failed to set clipboard text: {e}")),
                                    Err(e) => Err(format!("clipboard lock failed: {e}")),
                                }
                            } else {
                                Err("clipboard not available".to_string())
                            };

                            match result {
                                Ok(()) => {
                                    self.status_bar
                                        .set_notification(format!("copied {char_count} chars"));
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
                            && let Some(batch) = self.conversation.batches.get_mut(batch_idx)
                            && let Some(section) = batch.sections.get_mut(section_idx)
                        {
                            section.collapsed = !section.collapsed;
                            section.cached_height = None;
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
                // Scrolling always targets the conversation; switch focus first
                // if the input box is currently focused.
                if self.focus == Focus::Input {
                    self.focus = Focus::Conversation;
                }
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

        tracing::trace!(
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
                tracing::trace!("Row {} outside buffer area", row);
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
        tracing::trace!("Highlighted {} cells", cells_highlighted);
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

        // Global: Ctrl+L toggles the panel between its current content mode
        // and the Constellation view. If the panel is hidden it becomes
        // Visible (or Expanded on narrow terminals) at the same time.
        // Phase 6 T8.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
            self.panel_state.content = match self.panel_state.content {
                super::panel::PanelContent::Constellation => super::panel::PanelContent::Status,
                _ => super::panel::PanelContent::Constellation,
            };
            // Make sure the panel is actually visible if the user just
            // switched modes from a hidden panel.
            if self.panel_visibility == PanelVisibility::Hidden {
                self.panel_visibility = if self.terminal_width < MIN_PANEL_WIDTH {
                    PanelVisibility::Expanded
                } else {
                    PanelVisibility::Visible
                };
            }
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
                            // Accept the selected completion. Replacement
                            // strategy depends on completion mode.
                            if let Some(value) = self.autocomplete.accept() {
                                let value = value.to_string();
                                let replacement = match self.autocomplete.mode() {
                                    CompletionMode::Slash => format!("/{value} "),
                                    CompletionMode::Mention => {
                                        let text = self.input.current_text();
                                        replace_trailing_mention(&text, &value)
                                    }
                                };
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

                // Add user message to conversation. Snowflake IDs are
                // lex-sortable and safe for distributed minting — the daemon
                // uses this exact ID to tag all TurnEvents for this exchange.
                //
                // Phase 6 T8: outbound batches no longer carry a pre-decided
                // agent_name. The daemon's fronting resolver picks the
                // recipient and tags every event in the response with its
                // `TaggedTurnEvent.agent_id`; the conversation view sets the
                // batch's agent_name from the first response event.
                let batch_id = new_snowflake_id();
                let batch = RenderBatch::new(batch_id.clone(), Some(user_text));
                self.conversation.batches.push(batch);

                // Send to daemon if connected.
                if let Some(client) = &self.client {
                    // Phase 6 T8 routing precedence:
                    //   1. one-shot `/agent <id>` override (consumed here)
                    //   2. persistent `/front @<id>` route lock
                    //   3. default: Recipient::Auto (daemon's fronting resolver picks)
                    let recipient = if let Some(id) = self.pending_one_shot.take() {
                        Recipient::Direct(id)
                    } else if let Some(id) = self.route_lock.clone() {
                        Recipient::Direct(id)
                    } else {
                        Recipient::Auto
                    };
                    let client = client.clone();
                    let bid = batch_id;
                    let result_tx = self.result_tx.clone();
                    // Construct the Partner origin using this TUI's stable
                    // partner_id. The daemon does not mint partner IDs — each
                    // client supplies its own Author so that different callers
                    // (TUI, agent-to-agent, system services) are distinguishable
                    // in the agent's message history.
                    let origin = MessageOrigin::new(
                        Author::Partner(Partner {
                            user_id: self.partner_id.clone(),
                            display_name: self.partner_display_name.clone(),
                        }),
                        Sphere::Private,
                    );
                    tokio::spawn(async move {
                        tracing::debug!("sending message batch={bid} recipient={recipient:?}");
                        if let Err(e) = client
                            .send_message(bid.clone(), recipient, parts, origin)
                            .await
                        {
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
        use super::commands::CommandTarget;
        match self.command_registry.lookup(name).map(|e| e.target) {
            Some(CommandTarget::Local) => self.dispatch_local_command(name, args),
            Some(CommandTarget::Runtime) => self.dispatch_runtime_command(name, args),
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
    fn dispatch_local_command(&mut self, name: &str, args: &[String]) {
        match name {
            CMD_CLEAR => {
                self.conversation.batches.clear();
            }
            CMD_QUIT => {
                self.should_quit = true;
            }
            CMD_PANEL => {
                self.panel_visibility = self.panel_visibility.cycle();
            }
            CMD_PANE | CMD_FLOAT => match &self.zellij_state {
                ZellijState::InSession { .. } => {
                    let agent = args.first().map(|a| a.trim_start_matches('@').to_string());
                    match agent {
                        Some(agent) => {
                            let result = if name == CMD_PANE {
                                super::zellij::pane::spawn_tiled(&agent)
                            } else {
                                super::zellij::pane::spawn_floating(&agent)
                            };
                            if let Err(e) = result {
                                self.push_system_message(e);
                            }
                        }
                        None => {
                            self.push_system_message(format!("usage: /{name} @agent-name"));
                        }
                    }
                }
                _ => {
                    self.push_system_message(format!(
                        "/{name} requires a zellij session (not running inside zellij)"
                    ));
                }
            },
            _ => {}
        }
    }

    /// Handle a runtime command (requires daemon).
    fn dispatch_runtime_command(&mut self, name: &str, args: &[String]) {
        match name {
            CMD_FRONT => {
                // Phase 6 T8: `/front @<id>` sets a client-side persistent
                // route lock — every outbound message goes Direct(id) until
                // cleared. Bare `/front` clears the lock; subsequent sends
                // default to Recipient::Auto (daemon's fronting resolver
                // picks the destination). The persistent fronting set on the
                // daemon side is mutated via `SetFronting` RPC, not /front.
                if let Some(handle) = args.first() {
                    let canonical = if self.available_agents.is_empty() {
                        // No agent list cached (e.g. echo mode) — accept the
                        // user's input as-is.
                        SmolStr::from(handle.trim_start_matches('@'))
                    } else if let Some(c) = self.resolve_agent_handle(handle) {
                        c
                    } else {
                        let list = self.format_addressable_agents();
                        self.push_system_message(format!(
                            "unknown agent '{handle}'. available: {list}"
                        ));
                        return;
                    };
                    self.route_lock = Some(canonical.clone());
                    self.push_system_message(format!(
                        "route locked to {canonical}; clear with /front"
                    ));
                } else {
                    self.route_lock = None;
                    self.push_system_message(
                        "route lock cleared; outbound uses fronting resolver".to_string(),
                    );
                }
            }
            CMD_AGENT => {
                // Phase 6 T8: one-shot Recipient::Direct override for the
                // next outbound message. Cleared on use. Bare /agent is a
                // no-op (we could clear pending here, but there's no obvious
                // semantic for "clear an unfired one-shot").
                if let Some(handle) = args.first() {
                    let canonical = if self.available_agents.is_empty() {
                        SmolStr::from(handle.trim_start_matches('@'))
                    } else if let Some(c) = self.resolve_agent_handle(handle) {
                        c
                    } else {
                        let list = self.format_addressable_agents();
                        self.push_system_message(format!(
                            "unknown agent '{handle}'. available: {list}"
                        ));
                        return;
                    };
                    self.push_system_message(format!(
                        "next message will go directly to {canonical} (one-shot)"
                    ));
                    self.pending_one_shot = Some(canonical);
                } else {
                    self.push_system_message(
                        "/agent <id> sets a one-shot direct recipient for the next message"
                            .to_string(),
                    );
                }
            }
            CMD_PROMOTE => {
                // Phase 6 T8: /promote <id-or-name> → PromoteDraft RPC.
                let Some(handle) = args.first() else {
                    self.push_system_message(
                        "/promote <id> flips a Draft persona to Active".to_string(),
                    );
                    return;
                };
                match self.constellation_view.resolve_handle(handle) {
                    Err(e) => self.push_system_message(format!("/promote: {e}")),
                    Ok(persona_id) => {
                        if let Some(client) = &self.client {
                            let client = client.clone();
                            let result_tx = self.result_tx.clone();
                            tokio::spawn(async move {
                                match client.promote_draft(persona_id.to_string()).await {
                                    Ok(resp) if resp.success => {
                                        let _ = result_tx
                                            .send(format!("promoted {persona_id} to Active"));
                                    }
                                    Ok(resp) => {
                                        let _ = result_tx.send(format!(
                                            "promote failed: {}",
                                            resp.error.unwrap_or_default()
                                        ));
                                    }
                                    Err(e) => {
                                        let _ = result_tx.send(format!("promote RPC failed: {e}"));
                                    }
                                }
                            });
                        }
                    }
                }
            }
            CMD_RELATE => {
                // Phase 6 T8: /relate <from> <to> <kind> → AddRelationship RPC.
                let (from, to, kind) = match (args.first(), args.get(1), args.get(2)) {
                    (Some(f), Some(t), Some(k)) => (f, t, k),
                    _ => {
                        self.push_system_message(
                            "/relate <from> <to> <kind> adds a relationship edge".to_string(),
                        );
                        return;
                    }
                };
                let from_id = match self.constellation_view.resolve_handle(from) {
                    Ok(id) => id,
                    Err(e) => {
                        self.push_system_message(format!("/relate from: {e}"));
                        return;
                    }
                };
                let to_id = match self.constellation_view.resolve_handle(to) {
                    Ok(id) => id,
                    Err(e) => {
                        self.push_system_message(format!("/relate to: {e}"));
                        return;
                    }
                };
                // Accept both snake_case ("peer_with") and prose
                // ("peer with") at the call site; normalize to snake_case.
                let kind_norm = kind.replace(' ', "_").to_lowercase();
                if let Some(client) = &self.client {
                    let client = client.clone();
                    let result_tx = self.result_tx.clone();
                    let from_id_clone = from_id.clone();
                    let to_id_clone = to_id.clone();
                    let kind_clone = kind_norm.clone();
                    tokio::spawn(async move {
                        match client
                            .add_relationship(
                                from_id_clone.to_string(),
                                to_id_clone.to_string(),
                                kind_clone.clone(),
                            )
                            .await
                        {
                            Ok(resp) if resp.success => {
                                let _ = result_tx.send(format!(
                                    "relate: {from_id_clone} -[{kind_clone}]-> {to_id_clone}"
                                ));
                            }
                            Ok(resp) => {
                                let _ = result_tx.send(format!(
                                    "relate failed: {}",
                                    resp.error.unwrap_or_default()
                                ));
                            }
                            Err(e) => {
                                let _ = result_tx.send(format!("relate RPC failed: {e}"));
                            }
                        }
                    });
                }
            }
            CMD_AGENTS => {
                if let Some(client) = &self.client {
                    let client = client.clone();
                    let result_tx = self.result_tx.clone();
                    tokio::spawn(async move {
                        match client.list_agents().await {
                            Ok(agents) => {
                                let msg = if agents.is_empty() {
                                    "agents: (none active)".to_string()
                                } else {
                                    let lines: Vec<String> = agents
                                        .iter()
                                        .map(|a| format!("  {} ({})", a.agent_id, a.persona_name))
                                        .collect();
                                    format!("agents:\n{}", lines.join("\n"))
                                };
                                let _ = result_tx.send(msg);
                            }
                            Err(e) => {
                                let _ = result_tx.send(format!("/agents failed: {e}"));
                            }
                        }
                    });
                } else {
                    self.push_system_message("not connected to daemon.".into());
                }
            }
            CMD_STATUS => {
                if let Some(client) = &self.client {
                    let client = client.clone();
                    let result_tx = self.result_tx.clone();
                    tokio::spawn(async move {
                        match client.get_status().await {
                            Ok(status) => {
                                let msg = format!(
                                    "status: {} agent(s) active, uptime {}s",
                                    status.agent_count, status.uptime_secs
                                );
                                let _ = result_tx.send(msg);
                            }
                            Err(e) => {
                                let _ = result_tx.send(format!("/status failed: {e}"));
                            }
                        }
                    });
                } else {
                    self.push_system_message("not connected to daemon.".into());
                }
            }
            CMD_SHUTDOWN => {
                if let Some(client) = &self.client {
                    let client = client.clone();
                    let result_tx = self.result_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = client.shutdown().await {
                            let _ = result_tx.send(format!("shutdown failed: {e}"));
                        }
                    });
                    self.push_system_message("shutdown requested.".into());
                    self.should_quit = true;
                } else {
                    self.push_system_message("not connected to daemon.".into());
                }
            }
            CMD_CANCEL => {
                // Find the most recent streaming batch and cancel it.
                let batch_id = self
                    .conversation
                    .batches
                    .iter()
                    .rev()
                    .find(|b| b.streaming)
                    .map(|b| b.batch_id.clone());

                if let Some(batch_id) = batch_id {
                    if let Some(client) = &self.client {
                        let client = client.clone();
                        let result_tx = self.result_tx.clone();
                        let bid = batch_id.clone();
                        tokio::spawn(async move {
                            if let Err(e) = client.cancel_batch(bid.clone()).await {
                                let _ = result_tx.send(format!("cancel failed: {e}"));
                            }
                        });
                        self.push_system_message(format!("cancelling batch {batch_id}…"));
                    } else {
                        self.push_system_message("not connected to daemon.".into());
                    }
                } else {
                    self.push_system_message("no active response to cancel.".into());
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

    /// Return the number of conversation batches (system messages included).
    ///
    /// Used by integration tests to assert on conversation state without
    /// requiring access to private fields. The `dead_code` allow is needed
    /// because the binary target does not call these methods directly.
    #[allow(dead_code)]
    pub fn conversation_batch_count(&self) -> usize {
        self.conversation.batches.len()
    }

    /// Return the text content of the last section in the last conversation batch.
    ///
    /// Searches backwards through sections for the first `Display` or `Text`
    /// section and returns its text. Returns `None` when the conversation is
    /// empty or contains only non-text sections.
    ///
    /// Used by integration tests to verify user-facing error messages without
    /// inspecting internal model types directly.
    #[allow(dead_code)]
    pub fn last_conversation_message(&self) -> Option<&str> {
        let batch = self.conversation.batches.last()?;
        use super::model::SectionKind;
        for section in batch.sections.iter().rev() {
            match &section.kind {
                SectionKind::Display { text, .. } => return Some(text.as_str()),
                SectionKind::Text(text) => return Some(text.as_str()),
                _ => continue,
            }
        }
        None
    }

    /// Dispatch a slash command from a raw string.
    ///
    /// Parses `"/name arg1 arg2"` and routes through the normal command
    /// dispatch path. Used by integration tests to exercise slash command
    /// behaviour without requiring a running event loop.
    #[allow(dead_code)]
    pub fn dispatch_slash_command(&mut self, raw: &str) {
        let stripped = raw.strip_prefix('/').unwrap_or(raw);
        let mut parts = stripped.splitn(2, ' ');
        let name = parts.next().unwrap_or(stripped);
        let args: Vec<String> = parts
            .next()
            .map(|rest| rest.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        self.dispatch_command(name, &args);
    }

    /// Push a system message (note) into the conversation.
    ///
    /// Called by `run_chat()` to surface session init errors and other
    /// notifications as the first message before the event loop starts.
    /// Phase 6 T8: render the fronting state for the status bar.
    ///
    /// Precedence:
    /// 1. `route_lock` → "→ <locked-id>" (route lock overrides everything)
    /// 2. `fronting_active` non-empty → comma-joined names (with fallback in
    ///    parentheses if set and distinct)
    /// 3. `fronting_fallback` only → "fallback: <id>"
    /// 4. nothing → "no fronting configured"
    fn fronting_display_label(&self) -> String {
        if let Some(ref locked) = self.route_lock {
            return format!("→ {locked}");
        }
        if !self.fronting_active.is_empty() {
            let active = self
                .fronting_active
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return match self.fronting_fallback.as_ref() {
                Some(fb) if !self.fronting_active.iter().any(|a| a == fb) => {
                    format!("fronting: {active} (fallback: {fb})")
                }
                _ => format!("fronting: {active}"),
            };
        }
        if let Some(ref fb) = self.fronting_fallback {
            return format!("fallback: {fb}");
        }
        "no fronting configured".to_string()
    }

    pub fn push_system_message(&mut self, text: String) {
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
    pub fn load_history(&mut self, history: Vec<pattern_server::protocol::HistoricalBatch>) {
        let mut total_tokens = 0;
        for batch in history {
            total_tokens += batch.tokens;
            // Phase 6 T8: HistoricalBatch.agent_id labels each historical
            // batch with its responding agent, matching live batches tagged
            // from `TaggedTurnEvent.agent_id`.
            let mut render_batch = RenderBatch::new(batch.batch_id.clone(), batch.user_message)
                .with_agent(batch.agent_id.clone());
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
    ///
    /// Two trigger contexts:
    /// 1. Input starts with `/` and no space follows — slash-command name
    ///    completion. Replacement at accept replaces the whole input.
    /// 2. Input contains a trailing `@<partial>` token (most recent
    ///    `@` followed by characters matching a relaxed agent-handle
    ///    shape, no whitespace inside) — agent-mention completion.
    ///    Replacement at accept replaces just the trailing token.
    fn update_autocomplete(&mut self) {
        let text = self.input.current_text();

        // Slash-command context.
        if let Some(without_slash) = text.strip_prefix('/')
            && !without_slash.contains(' ')
        {
            let candidates = self.command_registry.candidates();
            self.autocomplete
                .update(without_slash, candidates, CompletionMode::Slash);
            return;
        }

        // Agent-mention context.
        if let Some(partial) = trailing_mention_partial(&text) {
            let candidates = self.agent_completion_candidates();
            if !candidates.is_empty() {
                self.autocomplete
                    .update(partial, &candidates, CompletionMode::Mention);
                return;
            }
        }

        self.autocomplete.hide();
    }

    /// Build (value, description) pairs for agent-mention completion.
    /// Includes both canonical agent ids and aliases. Aliases display
    /// `→ canonical_id` in the description so the user sees the resolution.
    fn agent_completion_candidates(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .available_agents
            .iter()
            .map(|id| (id.to_string(), "agent".to_string()))
            .collect();
        for (alias, canonical) in &self.agent_aliases {
            out.push((alias.to_string(), format!("→ {canonical}")));
        }
        out
    }

    /// Handle a tagged turn event from the daemon.
    ///
    /// `Display` events are routed to the panel (when visible) or toast
    /// popups (when hidden) instead of the conversation. All other events
    /// are pushed into the conversation batch as before.
    fn handle_daemon_event(&mut self, tagged: TaggedTurnEvent) {
        // Phase 6 T8: daemon-level notification events (agent_id="daemon")
        // route to fronting / constellation state, not to any batch.
        match &tagged.event {
            WireTurnEvent::FrontingChanged {
                active,
                fallback,
                rules,
            } => {
                let prev_active = self.fronting_active.clone();
                self.fronting_active = active.iter().map(|s| SmolStr::from(s.as_str())).collect();
                self.fronting_fallback = fallback.as_deref().map(SmolStr::from);
                self.fronting_rules = rules.clone();
                // Surface a one-line system note in the conversation when the
                // fronting set actually changed, so the user has context for
                // the next response coming from a different agent.
                if prev_active != self.fronting_active {
                    let prev_label = if prev_active.is_empty() {
                        "(none)".to_string()
                    } else {
                        prev_active
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let new_label = if self.fronting_active.is_empty() {
                        "(none)".to_string()
                    } else {
                        self.fronting_active
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    self.push_system_message(format!(
                        "fronting changed: {prev_label} → {new_label}"
                    ));
                }
                return;
            }
            WireTurnEvent::ConstellationChanged { .. } => {
                // Phase 6 T8: re-fetch the registry. Cheaper than tracking
                // per-mutation deltas; the registry list is small.
                self.refresh_constellation_view();
                return;
            }
            _ => {}
        }

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
            Some(b) => {
                // Phase 6 T8: outbound batches are created with no agent_name
                // (the daemon picks the recipient via fronting). The first
                // response event sets the attribution.
                if b.agent_name.is_none() && tagged.agent_id != "daemon" {
                    b.agent_name = Some(tagged.agent_id.clone());
                }
                b
            }
            None => {
                // New batch — create with no user message (the TUI set
                // the user message when it sent, above). Clear any stale
                // streaming display content from the previous batch so a
                // dropped connection mid-stream does not persist.
                self.panel_state.clear_display();
                let new_batch = RenderBatch::new(tagged.batch_id.clone(), None)
                    .with_agent(tagged.agent_id.clone());
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
        let input_lines = self.input.line_count() as u16;
        let layout = compute_layout_with_panel(
            frame.area(),
            self.panel_visibility,
            self.panel_pct,
            input_lines,
        );

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
            // Phase 6 T8: pre-render the constellation panel content into
            // owned `Text<'static>` so the StatefulWidget can borrow it.
            if self.panel_state.content == super::panel::PanelContent::Constellation {
                self.panel_state.constellation_text =
                    super::constellation_view::render_constellation_panel(
                        &self.constellation_view,
                        &self.fronting_active,
                        self.fronting_fallback.as_ref(),
                        &self.fronting_rules,
                        self.route_lock.as_ref(),
                    );
            }
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

        // Status bar — Phase 6 T8: shows current fronting state (active +
        // fallback) instead of a single locked agent. `route_lock` overrides
        // the display when set so users see who they've locked to.
        self.status_bar.persona_name = self.fronting_display_label();
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

/// If `text` ends with a token of the form `@<partial>` (the most recent
/// `@` followed by characters that look like an agent handle, with no
/// embedded whitespace), return the partial after the `@`. Otherwise
/// return `None`.
///
/// Triggers anywhere in the input — start, after whitespace, or
/// immediately after another delimiter character. Used to drive
/// agent-mention autocomplete.
fn trailing_mention_partial(text: &str) -> Option<&str> {
    let last_at = text.rfind('@')?;
    let after = &text[last_at + 1..];
    if after.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    // Require either start-of-input or whitespace before the `@` so that
    // tokens like `email@host` don't trigger.
    if last_at > 0 {
        let prev = text[..last_at].chars().next_back()?;
        if !prev.is_whitespace() {
            return None;
        }
    }
    Some(after)
}

/// Replace the trailing `@<partial>` token in `text` with `@<value>`.
/// If no trailing mention is present, appends `@<value>` to the input.
fn replace_trailing_mention(text: &str, value: &str) -> String {
    if let Some(idx) = text.rfind('@') {
        let after = &text[idx + 1..];
        if !after.chars().any(|c| c.is_whitespace()) {
            let mut out = text[..idx].to_string();
            out.push('@');
            out.push_str(value);
            return out;
        }
    }
    format!("{text}@{value}")
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

    // -----------------------------------------------------------------------
    // Trailing-mention parsing
    // -----------------------------------------------------------------------

    #[test]
    fn trailing_mention_at_start_of_input() {
        assert_eq!(trailing_mention_partial("@pat"), Some("pat"));
        assert_eq!(trailing_mention_partial("@"), Some(""));
    }

    #[test]
    fn trailing_mention_after_space() {
        assert_eq!(trailing_mention_partial("/front @pat"), Some("pat"));
        assert_eq!(trailing_mention_partial("hello @bob"), Some("bob"));
    }

    #[test]
    fn email_address_does_not_trigger_mention() {
        assert_eq!(trailing_mention_partial("user@host"), None);
    }

    #[test]
    fn trailing_whitespace_stops_mention_completion() {
        assert_eq!(trailing_mention_partial("@pat "), None);
        assert_eq!(trailing_mention_partial("@pat\n"), None);
    }

    #[test]
    fn no_at_returns_none() {
        assert_eq!(trailing_mention_partial("nothing here"), None);
        assert_eq!(trailing_mention_partial(""), None);
    }

    #[test]
    fn replace_trailing_mention_substitutes_partial() {
        assert_eq!(
            replace_trailing_mention("/front @pat", "pattern"),
            "/front @pattern"
        );
        assert_eq!(replace_trailing_mention("@p", "pattern"), "@pattern");
    }

    #[test]
    fn replace_trailing_mention_with_no_at_appends() {
        assert_eq!(replace_trailing_mention("hi", "pattern"), "hi@pattern");
    }

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
            mount_path: None,
        }
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
        batch.push_event(&WireTurnEvent::Text("The answer is **42**.".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        app.conversation.batches.push(batch);

        let output = render_app(&mut app, 60, 12);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn clear_command_empties_conversation() {
        let mut app = App::new();

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
        let mut app = App::new();
        assert!(!app.should_quit);

        app.dispatch_command("quit", &[]);
        assert!(app.should_quit);
    }

    #[test]
    fn unknown_command_shows_error() {
        let mut app = App::new();
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
        let mut app = App::new();
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
    fn front_command_sets_and_clears_route_lock() {
        let mut app = App::new();
        assert!(app.route_lock.is_none(), "default route_lock is None");

        app.dispatch_command("front", &["@supervisor".into()]);
        assert_eq!(
            app.route_lock.as_ref().map(|s| s.as_str()),
            Some("supervisor"),
            "/front @supervisor must set the route lock"
        );

        // Bare /front clears the lock.
        app.dispatch_command("front", &[]);
        assert!(
            app.route_lock.is_none(),
            "bare /front must clear the route lock"
        );

        // Should also push system messages confirming the changes.
        assert!(!app.conversation.batches.is_empty());
    }

    #[test]
    fn slash_command_from_input_dispatches() {
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
        app.panel_visibility = PanelVisibility::Visible;

        app.push_system_message("a system note".into());

        // Must be in conversation, not panel or toast.
        assert_eq!(app.conversation.batches.len(), 1);
        assert!(app.toast_state.is_empty());
        assert!(app.panel_state.notes.is_empty());
    }

    #[test]
    fn thinking_expand_to_panel() {
        let mut app = App::new();

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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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

    // -------------------------------------------------------------------
    // Phase 5: batch routing and cancel tests
    // -------------------------------------------------------------------

    #[test]
    fn events_route_to_correct_batch() {
        let mut app = App::new();

        // Pre-create two streaming batches.
        app.conversation
            .batches
            .push(RenderBatch::new("batch-A".into(), Some("first".into())));
        app.conversation
            .batches
            .push(RenderBatch::new("batch-B".into(), Some("second".into())));

        // Route a text event to batch-A.
        app.handle_daemon_event(tagged("batch-A", WireTurnEvent::Text("alpha".into())));
        // Route a text event to batch-B.
        app.handle_daemon_event(tagged("batch-B", WireTurnEvent::Text("beta".into())));

        let batch_a = app
            .conversation
            .batches
            .iter()
            .find(|b| b.batch_id == "batch-A")
            .expect("batch-A must exist");
        let batch_b = app
            .conversation
            .batches
            .iter()
            .find(|b| b.batch_id == "batch-B")
            .expect("batch-B must exist");

        assert_eq!(
            batch_a.sections.len(),
            1,
            "batch-A should have exactly one section"
        );
        assert_eq!(
            batch_b.sections.len(),
            1,
            "batch-B should have exactly one section"
        );

        // Verify content landed in the right place.
        match &batch_a.sections[0].kind {
            SectionKind::Text(text) => {
                assert!(
                    text.contains("alpha"),
                    "batch-A should contain 'alpha', got: {text}"
                );
            }
            other => panic!("batch-A: expected Text, got {other:?}"),
        }
        match &batch_b.sections[0].kind {
            SectionKind::Text(text) => {
                assert!(
                    text.contains("beta"),
                    "batch-B should contain 'beta', got: {text}"
                );
            }
            other => panic!("batch-B: expected Text, got {other:?}"),
        }
    }

    #[test]
    fn no_cross_contamination_between_batches() {
        let mut app = App::new();

        // Interleave events for two different batches.
        app.handle_daemon_event(tagged("batch-X", WireTurnEvent::Text("x1".into())));
        app.handle_daemon_event(tagged("batch-Y", WireTurnEvent::Text("y1".into())));
        app.handle_daemon_event(tagged("batch-X", WireTurnEvent::Text("x2".into())));
        app.handle_daemon_event(tagged("batch-Y", WireTurnEvent::Text("y2".into())));

        assert_eq!(app.conversation.batches.len(), 2);

        let batch_x = app
            .conversation
            .batches
            .iter()
            .find(|b| b.batch_id == "batch-X")
            .expect("batch-X must exist");
        let batch_y = app
            .conversation
            .batches
            .iter()
            .find(|b| b.batch_id == "batch-Y")
            .expect("batch-Y must exist");

        // Both batches should only have one section (text events accumulate).
        assert_eq!(batch_x.sections.len(), 1);
        assert_eq!(batch_y.sections.len(), 1);

        match &batch_x.sections[0].kind {
            SectionKind::Text(text) => {
                assert!(
                    text.contains("x1") && text.contains("x2"),
                    "batch-X should contain both x events, got: {text}"
                );
                assert!(
                    !text.contains("y1") && !text.contains("y2"),
                    "batch-X must not contain Y events, got: {text}"
                );
            }
            other => panic!("batch-X: expected Text, got {other:?}"),
        }
        match &batch_y.sections[0].kind {
            SectionKind::Text(text) => {
                assert!(
                    text.contains("y1") && text.contains("y2"),
                    "batch-Y should contain both y events, got: {text}"
                );
                assert!(
                    !text.contains("x1") && !text.contains("x2"),
                    "batch-Y must not contain X events, got: {text}"
                );
            }
            other => panic!("batch-Y: expected Text, got {other:?}"),
        }
    }

    #[test]
    fn unknown_batch_id_creates_new_batch() {
        let mut app = App::new();
        assert!(app.conversation.batches.is_empty());

        // Event arrives for a batch-id the TUI has never seen.
        app.handle_daemon_event(tagged("daemon-side-only", WireTurnEvent::Text("hi".into())));

        assert_eq!(app.conversation.batches.len(), 1);
        assert_eq!(app.conversation.batches[0].batch_id, "daemon-side-only");
    }

    // -------------------------------------------------------------------
    // Command dispatch tests: /agents, /status, /shutdown
    // -------------------------------------------------------------------

    /// `/agents` without a daemon connection surfaces "not connected" immediately.
    #[test]
    fn agents_command_without_client_shows_not_connected() {
        let mut app = App::new();
        // No client set — dispatch_runtime_command should push a system message.
        app.dispatch_runtime_command("agents", &[]);
        assert_eq!(app.conversation.batches.len(), 1);
        let msg = app.last_conversation_message().unwrap_or("");
        assert!(
            msg.contains("not connected"),
            "expected 'not connected' message, got: {msg}"
        );
    }

    /// `/status` without a daemon connection surfaces "not connected" immediately.
    #[test]
    fn status_command_without_client_shows_not_connected() {
        let mut app = App::new();
        app.dispatch_runtime_command("status", &[]);
        assert_eq!(app.conversation.batches.len(), 1);
        let msg = app.last_conversation_message().unwrap_or("");
        assert!(
            msg.contains("not connected"),
            "expected 'not connected' message, got: {msg}"
        );
    }

    /// `/agents` with a real echo-mode daemon calls `list_agents()` and renders
    /// the result as a system message in the conversation. Verifies the Phase 3
    /// spec: "Command dispatch test verifying /agents calls client.list_agents()
    /// and renders result as system message."
    #[tokio::test]
    async fn agents_command_calls_list_agents_and_renders_result() {
        use pattern_server::server::DaemonServer;

        let handle = DaemonServer::spawn();
        let raw_client = handle.client;
        let client = pattern_server::client::DaemonClient::from_local(raw_client);

        // Replace the placeholder channel with a real one owned in this scope.
        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let mut app = App::new();
        app.client = Some(client);
        app.result_tx = result_tx;

        // Dispatch /agents — spawns a task that will send to result_tx.
        app.dispatch_runtime_command("agents", &[]);

        // Wait for the spawned task to complete and send its result.
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), result_rx.recv())
            .await
            .expect("timed out waiting for /agents result")
            .expect("channel closed unexpectedly");

        // In echo mode the daemon returns an empty agent list.
        assert!(
            msg.contains("agents:") || msg.contains("(none active)"),
            "/agents result should contain agent list, got: {msg}"
        );
    }

    /// `/status` with a real echo-mode daemon calls `get_status()` and renders
    /// uptime + agent count as a system message.
    #[tokio::test]
    async fn status_command_calls_get_status_and_renders_result() {
        use pattern_server::server::DaemonServer;

        let handle = DaemonServer::spawn();
        let raw_client = handle.client;
        let client = pattern_server::client::DaemonClient::from_local(raw_client);

        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let mut app = App::new();
        app.client = Some(client);
        app.result_tx = result_tx;

        app.dispatch_runtime_command("status", &[]);

        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), result_rx.recv())
            .await
            .expect("timed out waiting for /status result")
            .expect("channel closed unexpectedly");

        assert!(
            msg.contains("status:") && msg.contains("uptime"),
            "/status result should contain status info, got: {msg}"
        );
    }

    /// `/shutdown` with a real echo-mode daemon calls `shutdown()` (not
    /// `run_command("shutdown", ...)`), sets `should_quit`, and the daemon's
    /// Shutdown handler responds cleanly.
    #[tokio::test]
    async fn shutdown_command_calls_shutdown_rpc_and_sets_quit() {
        use pattern_server::server::DaemonServer;

        let handle = DaemonServer::spawn();
        let raw_client = handle.client;
        let client = pattern_server::client::DaemonClient::from_local(raw_client);

        let (result_tx, _result_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let mut app = App::new();
        app.client = Some(client);
        app.result_tx = result_tx;

        assert!(!app.should_quit);
        app.dispatch_runtime_command("shutdown", &[]);
        // should_quit is set synchronously before the async task completes.
        assert!(
            app.should_quit,
            "/shutdown should set should_quit immediately"
        );
    }

    #[test]
    fn cancel_command_with_no_streaming_batch() {
        let mut app = App::new();

        // Add a non-streaming batch (already finished).
        let mut batch = RenderBatch::new("batch-1".into(), Some("hello".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        batch.streaming = false;
        app.conversation.batches.push(batch);

        let batch_count_before = app.conversation.batches.len();
        app.dispatch_runtime_command("cancel", &[]);

        // Should add one system message explaining there's nothing to cancel.
        assert_eq!(app.conversation.batches.len(), batch_count_before + 1);
        let sys_batch = app.conversation.batches.last().unwrap();
        match &sys_batch.sections[0].kind {
            super::super::model::SectionKind::Display { text, .. } => {
                assert!(
                    text.contains("no active response to cancel"),
                    "expected no-op message, got: {text}"
                );
            }
            other => panic!("expected Display section, got {other:?}"),
        }
    }

    #[test]
    fn cancel_command_targets_most_recent_streaming_batch() {
        let mut app = App::new();
        // No client, so the cancel path hits the "not connected" branch.
        // We just verify it finds the correct streaming batch.

        // Finished batch.
        let mut done = RenderBatch::new("done".into(), Some("old".into()));
        done.streaming = false;
        app.conversation.batches.push(done);

        // Active streaming batch.
        let mut active = RenderBatch::new("active".into(), Some("new".into()));
        active.streaming = true;
        app.conversation.batches.push(active);

        let batch_count_before = app.conversation.batches.len();
        app.dispatch_runtime_command("cancel", &[]);

        // The cancel path without a client should push "not connected" message,
        // which means it DID find the streaming batch (entered the Some branch).
        assert_eq!(app.conversation.batches.len(), batch_count_before + 1);
        let sys_batch = app.conversation.batches.last().unwrap();
        match &sys_batch.sections[0].kind {
            super::super::model::SectionKind::Display { text, .. } => {
                assert!(
                    text.contains("not connected"),
                    "expected 'not connected' message (found streaming batch but no client), got: {text}"
                );
            }
            other => panic!("expected Display section, got {other:?}"),
        }
    }
}
