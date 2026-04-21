# v3 TUI — Phase 3: Input and slash commands

**Goal:** Multi-line input with message submission, slash command parsing/dispatch, input history, and fuzzy autocomplete for commands.

**Architecture:** The input handler wraps ratatui-textarea, intercepting Enter (submit) vs Shift+Enter (newline) and detecting `/` prefixes for slash commands. A command registry defines available commands with metadata and argument type hints. nucleo provides fuzzy matching for autocomplete, with a `CompletionSource` trait that supports pluggable sources (commands now, agent names and history search later). The TUI sends `Vec<ContentPart>` to the daemon — the daemon constructs `TurnInput` with batch IDs, origin, etc.

**Tech Stack:** ratatui-textarea 0.9.1, nucleo (fuzzy matching), pattern-server (IRPC client)

**Scope:** Phase 3 of 6 from the v3-tui design plan.

**Codebase verified:** 2026-04-20

---

## Acceptance criteria coverage

This phase implements and tests:

### v3-tui.AC3: Input and slash commands
- **v3-tui.AC3.1 Success:** Enter submits message; shift/ctrl+enter inserts newline; multi-line input works
- **v3-tui.AC3.2 Success:** `/agents` returns agent list from daemon; rendered in conversation or panel
- **v3-tui.AC3.3 Success:** `/front @agent-name` changes fronting persona; status bar updates; subsequent messages go to new front
- **v3-tui.AC3.4 Success:** `/clear` clears conversation view without affecting daemon state
- **v3-tui.AC3.5 Success:** `/quit` exits the TUI cleanly without stopping the daemon
- **v3-tui.AC3.6 Success:** Up arrow cycles through previous message inputs
- **v3-tui.AC3.7 Failure:** Unknown slash command shows "unknown command" error inline, doesn't crash
- **v3-tui.AC3.8 Edge:** Plugin-namespaced command `/plugin-name:cmd` forwards to daemon and returns result

---

<!-- START_TASK_1 -->
### Task 1: Add nucleo dependency and update protocol type

**Files:**
- Modify: `Cargo.toml` (workspace root — add nucleo)
- Modify: `crates/pattern_cli/Cargo.toml` (add nucleo)
- Modify: `crates/pattern_server/src/protocol.rs` (change AgentMessage to use Vec<ContentPart>)
- Modify: `docs/implementation-plans/2026-04-19-v3-tui/phase_01.md` (update AgentMessage definition)

**Step 1: Add workspace dep**

In root `Cargo.toml` `[workspace.dependencies]`:
```toml
nucleo = "0.5"
```

In `crates/pattern_cli/Cargo.toml` `[dependencies]`:
```toml
nucleo = { workspace = true }
```

**Step 2: Verify AgentMessage uses Vec<ContentPart>**

`AgentMessage` was already updated to use `parts: Vec<ContentPart>` in Phase 1 Task 3. Verify the protocol definition matches — no changes needed here.

**Step 3: Update phase_01.md**

Replace the `AgentMessage` definition in the protocol task to match.

**Step 4: Verify**

Run: `cargo check -p pattern-server && cargo check -p pattern-cli`
Expected: compiles

Run: `cargo nextest run -p pattern-server`
Expected: existing tests pass (update test assertions for new field name)

**Commit:** `[pattern-server] AgentMessage carries Vec<ContentPart> instead of String`
<!-- END_TASK_1 -->

<!-- START_SUBCOMPONENT_A (tasks 2-3) -->
<!-- START_TASK_2 -->
### Task 2: Slash command registry

**Files:**
- Create: `crates/pattern_cli/src/tui/commands.rs`

**Implementation:**

The command registry is a static data structure defining all available slash commands. Each command has metadata used for dispatch and autocomplete.

```rust
/// Where the command is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTarget {
    /// Handled locally by the TUI (no daemon call).
    Local,
    /// Forwarded to the daemon's runtime.
    Runtime,
}

/// What kind of argument a command expects (for autocomplete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgHint {
    /// No arguments.
    None,
    /// Agent name (completable from daemon's agent list).
    AgentName,
    /// Free-form text.
    FreeText,
}

#[derive(Debug, Clone)]
pub struct CommandDef {
    pub name: &'static str,
    pub description: &'static str,
    pub target: CommandTarget,
    pub arg_hint: ArgHint,
}

/// All built-in commands.
pub fn builtin_commands() -> &'static [CommandDef] {
    &[
        CommandDef { name: "clear",    description: "Clear conversation view",      target: CommandTarget::Local,   arg_hint: ArgHint::None },
        CommandDef { name: "quit",     description: "Exit the TUI",                 target: CommandTarget::Local,   arg_hint: ArgHint::None },
        CommandDef { name: "panel",    description: "Toggle side panel",            target: CommandTarget::Local,   arg_hint: ArgHint::None },
        CommandDef { name: "expand",   description: "Expand focused section",       target: CommandTarget::Local,   arg_hint: ArgHint::None },
        CommandDef { name: "front",    description: "Switch fronting persona",      target: CommandTarget::Runtime, arg_hint: ArgHint::AgentName },
        CommandDef { name: "agents",   description: "List active agents",           target: CommandTarget::Runtime, arg_hint: ArgHint::None },
        CommandDef { name: "status",   description: "Show runtime status",          target: CommandTarget::Runtime, arg_hint: ArgHint::None },
        CommandDef { name: "context",  description: "Show context/memory info",     target: CommandTarget::Runtime, arg_hint: ArgHint::None },
        CommandDef { name: "shutdown", description: "Stop the daemon",              target: CommandTarget::Runtime, arg_hint: ArgHint::None },
    ]
}
```

Parser function:
```rust
/// Parse a slash command string into (command_name, args).
/// Returns None if the string doesn't start with '/'.
pub fn parse_slash_command(input: &str) -> Option<(&str, Vec<&str>)> {
    let input = input.trim();
    let without_slash = input.strip_prefix('/')?;
    let mut parts = without_slash.split_whitespace();
    let command = parts.next()?;
    let args: Vec<&str> = parts.collect();
    Some((command, args))
}

/// Look up a command by name. Supports plugin-namespaced commands (e.g., "plugin:cmd").
pub fn lookup_command(name: &str) -> Option<&'static CommandDef> {
    builtin_commands().iter().find(|c| c.name == name)
}
```

**Testing:**

- `parse_slash_command_basic` — `/quit` → `("quit", [])`
- `parse_slash_command_with_args` — `/front @supervisor` → `("front", ["@supervisor"])`
- `parse_slash_command_not_slash` — `"hello"` → `None`
- `parse_slash_command_namespaced` — `/plugin:cmd arg` → `("plugin:cmd", ["arg"])`
- `lookup_command_found` — `"clear"` → `Some(CommandDef { target: Local, .. })`
- `lookup_command_not_found` — `"nonexistent"` → `None`

**Verification:**

Run: `cargo nextest run -p pattern-cli commands`
Expected: all tests pass

**Commit:** `[pattern-cli] slash command registry and parser`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Input handler with history

**Verifies:** v3-tui.AC3.1, v3-tui.AC3.6

**Files:**
- Create: `crates/pattern_cli/src/tui/input.rs`
- Modify: `crates/pattern_cli/src/tui/mod.rs` (add module)

**Implementation:**

Wraps `TextArea` with submit/history/slash-command detection. The handler intercepts key events before passing them to the textarea.

```rust
use ratatui_textarea::{TextArea, Input, Key};
use pattern_core::types::provider::ContentPart;

/// Result of processing an input event.
pub enum InputAction {
    /// User submitted a message (Enter with non-empty input).
    Submit(Vec<ContentPart>),
    /// User entered a slash command.
    SlashCommand { name: String, args: Vec<String> },
    /// Input changed (for autocomplete refresh).
    Changed,
    /// No action needed.
    None,
}

pub struct InputHandler {
    textarea: TextArea<'static>,
    history: Vec<String>,
    history_index: Option<usize>,
    max_history: usize,
    /// Stashed current input when browsing history.
    stashed_input: Option<String>,
}
```

Key event handling:
- `Input { key: Key::Enter, shift: false, ctrl: false, .. }` → check for slash command prefix, otherwise submit as `Vec<ContentPart>` (text content part). Push to history, clear textarea.
- `Input { key: Key::Enter, shift: true, .. }` or `Input { key: Key::Enter, ctrl: true, .. }` → `textarea.insert_newline()`
- `Input { key: Key::Up, .. }` when textarea has single empty line → cycle to previous history entry. Stash current input on first Up press.
- `Input { key: Key::Down, .. }` when browsing history → cycle forward, restore stashed input at end.
- `Input { key: Key::Escape, .. }` → clear input, cancel history browsing.
- All other inputs → pass to `textarea.input()`, return `Changed`.

Submit logic:
```rust
fn submit(&mut self) -> InputAction {
    let text = self.textarea.lines().join("\n").trim().to_string();
    if text.is_empty() {
        return InputAction::None;
    }

    // Push to history.
    self.history.push(text.clone());
    if self.history.len() > self.max_history {
        self.history.remove(0);
    }
    self.history_index = None;
    self.stashed_input = None;
    self.textarea.select_all();
    self.textarea.cut(); // Clear the textarea.

    // Check for slash command.
    if let Some((name, args)) = parse_slash_command(&text) {
        return InputAction::SlashCommand {
            name: name.to_string(),
            args: args.into_iter().map(String::from).collect(),
        };
    }

    InputAction::Submit(vec![ContentPart::Text(text)])
}
```

History cycling:
```rust
fn history_up(&mut self) {
    if self.history.is_empty() { return; }
    let idx = match self.history_index {
        Some(i) if i > 0 => i - 1,
        Some(_) => return, // Already at oldest.
        None => {
            // Stash current input before entering history.
            self.stashed_input = Some(self.textarea.lines().join("\n"));
            self.history.len() - 1
        }
    };
    self.history_index = Some(idx);
    self.textarea.select_all();
    self.textarea.cut();
    self.textarea.insert_str(&self.history[idx]);
}

fn history_down(&mut self) {
    let idx = match self.history_index {
        Some(i) => i + 1,
        None => return,
    };
    if idx >= self.history.len() {
        // Restore stashed input.
        self.history_index = None;
        self.textarea.select_all();
        self.textarea.cut();
        if let Some(stashed) = self.stashed_input.take() {
            self.textarea.insert_str(&stashed);
        }
    } else {
        self.history_index = Some(idx);
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(&self.history[idx]);
    }
}
```

**Testing:**

- `enter_submits_text` — type "hello", Enter → `Submit(vec![ContentPart::Text("hello")])`
- `shift_enter_inserts_newline` — Shift+Enter → textarea has two lines, no submit
- `slash_command_detected` — type "/quit", Enter → `SlashCommand { name: "quit", args: [] }`
- `history_up_cycles` — submit "a", submit "b", Up → textarea shows "b", Up → "a"
- `history_down_restores` — submit "a", Up (shows "a"), Down → textarea empty
- `history_stashes_current_input` — type "draft", Up → shows last history, Down → "draft" restored
- `empty_enter_does_nothing` — Enter with empty textarea → `None`
- `history_max_size` — push 60 entries with max 50 → oldest 10 dropped

**Verification:**

Run: `cargo nextest run -p pattern-cli input`
Expected: all tests pass

**Commit:** `[pattern-cli] input handler with submit, newline, and history cycling`
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->
<!-- START_TASK_4 -->
### Task 4: Autocomplete widget with nucleo

**Verifies:** (no AC — autocomplete is a UX enhancement beyond the design plan's explicit criteria, but supports AC3.2, AC3.3, AC3.8 by making commands discoverable)

**Files:**
- Create: `crates/pattern_cli/src/tui/autocomplete.rs`

**Implementation:**

A completion popup powered by nucleo fuzzy matching with pluggable sources.

```rust
/// A candidate for autocomplete display.
pub struct CompletionItem {
    pub value: String,
    pub description: String,
    pub score: u32,
}

/// Trait for pluggable completion sources.
pub trait CompletionSource {
    /// Return all candidates. The autocomplete widget handles filtering via nucleo.
    fn candidates(&self) -> Vec<(String, String)>; // (value, description)
}
```

`CommandSource` implements `CompletionSource` — returns all commands from the registry.

`AutocompleteState`:
- `visible: bool`
- `items: Vec<CompletionItem>` — filtered and scored by nucleo
- `selected: usize` — index into items
- `pattern: String` — current input being matched against

`AutocompleteWidget` renders as a `List` positioned above the input area:
- Calculate popup height: `min(items.len(), 8)` lines
- Position: directly above the input area rect, full width
- Render with `Clear` widget first (to overwrite conversation content behind the popup), then `List` with highlight on selected item
- Each item shows: command name (left-aligned), description (right-aligned, dimmed)

Key handling when autocomplete is visible:
- Tab / Down → select next
- Shift+Tab / Up → select previous
- Enter → accept completion (replace input with selected value + trailing space)
- Escape → dismiss popup
- Any other key → pass to textarea, update pattern, re-filter

nucleo integration:
```rust
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};

fn filter_candidates(
    pattern: &str,
    candidates: &[(String, String)],
) -> Vec<CompletionItem> {
    let mut matcher = Matcher::new(nucleo::Config::DEFAULT);
    let pat = Pattern::parse(pattern, CaseMatching::Ignore, Normalization::Smart);

    let mut results: Vec<CompletionItem> = candidates
        .iter()
        .filter_map(|(value, desc)| {
            let mut buf = Vec::new();
            let score = pat.score(nucleo::Utf32Str::new(value, &mut buf), &mut matcher)?;
            Some(CompletionItem {
                value: value.clone(),
                description: desc.clone(),
                score,
            })
        })
        .collect();

    results.sort_by(|a, b| b.score.cmp(&a.score));
    results
}
```

Activation logic:
- Popup shows when input starts with `/` and has at least one character after the slash
- Pattern is the text after `/` (e.g., `/fro` → pattern is `fro`)
- If no matches, popup stays hidden
- If exactly one match and it equals the input, popup auto-dismisses (already complete)

**Testing:**

- `filter_matches_prefix` — pattern "cl" matches "clear" with high score
- `filter_fuzzy_matches` — pattern "sht" matches "shutdown"
- `filter_no_match` — pattern "xyz" returns empty
- `filter_sorts_by_score` — best matches first
- `accept_replaces_input` — selecting "clear" replaces textarea with "/clear "
- `escape_dismisses` — Escape hides popup
- Snapshot test: popup rendered above input area with 3 matching commands

**Verification:**

Run: `cargo nextest run -p pattern-cli autocomplete`
Expected: all tests pass

**Commit:** `[pattern-cli] fuzzy autocomplete widget with nucleo`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Command dispatch and daemon interaction

**Verifies:** v3-tui.AC3.2, v3-tui.AC3.3, v3-tui.AC3.4, v3-tui.AC3.5, v3-tui.AC3.7, v3-tui.AC3.8

**Files:**
- Modify: `crates/pattern_cli/src/tui/app.rs`

**Implementation:**

Wire `InputAction` variants into the app's event loop. The app now holds an optional `DaemonClient` and the current fronting agent ID.

New fields on `App`:
```rust
client: Option<DaemonClient>,
current_agent: SmolStr, // default agent ID, updated by /front
```

Dispatch logic:
```rust
fn handle_input_action(&mut self, action: InputAction) {
    match action {
        InputAction::Submit(parts) => {
            // Add user message to conversation.
            let batch_id = new_snowflake_id();
            let mut batch = RenderBatch::new(batch_id.clone());
            // Render user message from parts (extract text for display).
            batch.user_message = Some(text_from_parts(&parts));
            self.conversation.batches.push(batch);

            // Send to daemon if connected.
            if let Some(client) = &self.client {
                let agent_id = self.current_agent.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    if let Err(e) = client.send_message(agent_id, parts).await {
                        // TODO: surface error to conversation.
                        tracing::error!("send failed: {e}");
                    }
                });
            }
        }
        InputAction::SlashCommand { name, args } => {
            self.dispatch_command(&name, &args);
        }
        InputAction::Changed => {
            // Update autocomplete if visible.
            self.update_autocomplete();
        }
        InputAction::None => {}
    }
}
```

Local command handlers:
- `clear` → `self.conversation.batches.clear()`
- `quit` → `self.should_quit = true`
- `panel` → toggle panel state (Phase 4 implements the panel; this sets the flag)
- `expand` → toggle focused section expanded state

Runtime command handlers:
- `agents` → spawn async `client.list_agents()`, render result as a system message in conversation
- `status` → spawn async `client.get_status()`, render result
- `front` → validate agent name, call `client.run_command("front", args)`, update `self.current_agent` and status bar
- `context` → forward to daemon
- `shutdown` → call daemon stop, set `should_quit = true`

Unknown commands and plugin-namespaced commands:
- If `name` contains `:` (e.g., `plugin:cmd`), forward to daemon via `client.run_command(name, args)` (AC3.8)
- If not found and no `:`, render inline error: `"unknown command: /{name}. Type / for available commands."` (AC3.7)

Auto-start daemon:
```rust
// On first Submit or Runtime command, if client is None:
// ensure_daemon_running: spawn pattern-server start, then poll for state file
// every 100ms up to 5 seconds. If state file appears and PID is alive, connect.
if self.client.is_none() {
    match ensure_daemon_running().await {
        Ok(client) => {
            // Subscribe to output.
            if let Ok(rx) = client.subscribe_output(self.current_agent.clone()).await {
                self.event_rx = Some(rx);
            }
            self.client = Some(client);
        }
        Err(e) => {
            self.push_system_message(format!("failed to start daemon: {e}"));
        }
    }
}
```

Helper to push system messages (command output, errors) into conversation:
```rust
fn push_system_message(&mut self, text: String) {
    let mut batch = RenderBatch::new(new_snowflake_id());
    batch.push_event(&TurnEvent::Display {
        kind: DisplayKind::Note,
        text,
    });
    batch.streaming = false;
    self.conversation.batches.push(batch);
}
```

Update `DaemonClient::send_message` signature to accept `Vec<ContentPart>`:
```rust
pub async fn send_message(&self, agent_id: SmolStr, parts: Vec<ContentPart>) -> Result<SmolStr>
```

**Testing:**

- `clear_command_empties_conversation` — push batches, dispatch "clear", batches empty
- `quit_command_sets_should_quit` — dispatch "quit", `should_quit` is true
- `unknown_command_shows_error` — dispatch "nonexistent", last batch has error note
- `namespaced_command_forwarded` — dispatch "plugin:cmd" with mock client, verify `run_command` called
- `submit_creates_batch_with_user_message` — submit text, new batch has user_message set
- `front_command_updates_current_agent` — dispatch "front @test", `current_agent` updated

**Verification:**

Run: `cargo nextest run -p pattern-cli app`
Expected: all tests pass

**Commit:** `[pattern-cli] command dispatch and daemon interaction`
<!-- END_TASK_5 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_TASK_6 -->
### Task 6: Wire autocomplete into event loop and render

**Verifies:** v3-tui.AC3.1

**Files:**
- Modify: `crates/pattern_cli/src/tui/app.rs` (render + key routing)
- Modify: `crates/pattern_cli/src/tui/input.rs` (autocomplete trigger)

**Implementation:**

Integrate autocomplete state into the app's render and event handling.

In `App`:
```rust
autocomplete: AutocompleteState,
command_source: CommandSource,
```

On each `InputAction::Changed`:
```rust
fn update_autocomplete(&mut self) {
    let text = self.input_handler.current_text();
    if let Some(without_slash) = text.strip_prefix('/') {
        if !without_slash.is_empty() && !without_slash.contains(' ') {
            // Completing command name.
            let candidates = self.command_source.candidates();
            self.autocomplete.update(without_slash, &candidates);
            return;
        }
    }
    self.autocomplete.hide();
}
```

Key routing when autocomplete is visible — intercept before passing to textarea:
- Tab, Down → `autocomplete.next()`
- Shift+Tab, Up → `autocomplete.prev()`
- Enter → accept: replace textarea content with `/{selected.value} `, hide autocomplete
- Escape → hide autocomplete, don't clear textarea
- Other → pass to textarea, then `update_autocomplete()`

Render order in `App::draw()`:
1. Compute layout (conversation, input, status bar)
2. Render conversation
3. Render textarea in input area
4. Render status bar
5. If autocomplete visible: render popup *on top* of conversation area (positioned just above input). Use `Clear` widget to erase the area, then render `List`.

**Testing:**

- Snapshot test: `autocomplete_popup_above_input` — typing `/cl` shows popup with "clear" highlighted
- `tab_cycles_selection` — Tab moves selection down, wraps around
- `enter_accepts_and_replaces_input` — Enter on "clear" → textarea shows "/clear "
- `escape_hides_popup` — Escape → popup not visible
- `space_after_command_hides_popup` — typing "/front " (with space) → popup hidden (now in arg mode)

**Verification:**

Run: `cargo nextest run -p pattern-cli`
Expected: all tests pass

**Commit:** `[pattern-cli] wire autocomplete into event loop and render`
<!-- END_TASK_6 -->
