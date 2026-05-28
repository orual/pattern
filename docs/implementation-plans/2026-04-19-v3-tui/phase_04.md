# v3 TUI — Phase 4: Side panel and display lane

**Goal:** A collapsible side panel with switchable content, display event routing, a status bar, and clipboard support.

**Architecture:** The panel has three states: Hidden (zero chrome, full-width conversation), Visible (conversation + panel with single-column divider), Expanded (full-width panel). Display events route to the panel when visible, or appear as toast-style popups when the panel is hidden. The status bar shows fronting persona, agent count, and context usage. Clipboard uses OSC 52 for remote-compatible copy with arboard as local fallback.

**Tech Stack:** ratatui 0.30, tui-popup 0.7, arboard (clipboard)

**Scope:** Phase 4 of 6 from the v3-tui design plan.

**Codebase verified:** 2026-04-20

---

## Acceptance criteria coverage

This phase implements and tests:

### v3-tui.AC4: Side panel and display
- **v3-tui.AC4.1 Success:** Panel has three states: hidden (zero chrome, full-width conversation), visible (conversation + panel with minimal divider), expanded (full-width panel). `/panel` cycles states; Ctrl+P does the same
- **v3-tui.AC4.2 Success:** `TurnEvent::Display(Note)` renders in the panel's status area, NOT in the conversation view
- **v3-tui.AC4.3 Success:** `TurnEvent::Display(Chunk/Final)` renders in the panel content area
- **v3-tui.AC4.4 Success:** Status bar shows: fronting persona name, active agent count, context token usage
- **v3-tui.AC4.5 Success:** Expanding a thinking block "in panel" shows the full content in the side panel without changing conversation scroll position
- **v3-tui.AC4.6 Edge:** Terminal width below threshold auto-hides panel; panel toggle is a no-op until width sufficient
- **v3-tui.AC4.7 Edge:** Panel width resizable via keybinding or drag (if terminal supports mouse)
- **v3-tui.AC4.8 Success:** Selection mode (keybinding to enter) allows mouse drag to select text content in conversation area; selected text copied to clipboard via OSC 52
- **v3-tui.AC4.9 Success:** In hidden panel state, conversation area has zero non-text chrome on left and right edges — terminal-native text selection works cleanly

---

<!-- START_TASK_1 -->
### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/pattern_cli/Cargo.toml`

**Step 1: Add workspace deps**

```toml
tui-popup = "0.7"
arboard = "3"
base64 = "0.22"
```

**Step 2: Add to pattern_cli**

```toml
tui-popup = { workspace = true }
arboard = { workspace = true }
base64 = { workspace = true }
```

**Step 3: Verify**

Run: `cargo check -p pattern-cli`
Expected: compiles

**Commit:** `[pattern-cli] add tui-popup, arboard, base64 for panel and clipboard`
<!-- END_TASK_1 -->

<!-- START_SUBCOMPONENT_A (tasks 2-3) -->
<!-- START_TASK_2 -->
### Task 2: Panel state and layout update

**Verifies:** v3-tui.AC4.1, v3-tui.AC4.6, v3-tui.AC4.9

**Files:**
- Modify: `crates/pattern_cli/src/tui/layout.rs`

**Implementation:**

Extend the existing `TuiLayout` to support three panel states and horizontal splitting.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelVisibility {
    #[default]
    Hidden,
    Visible,
    Expanded,
}

impl PanelVisibility {
    /// Cycle: Hidden → Visible → Expanded → Hidden.
    pub fn cycle(self) -> Self {
        match self {
            PanelVisibility::Hidden => PanelVisibility::Visible,
            PanelVisibility::Visible => PanelVisibility::Expanded,
            PanelVisibility::Expanded => PanelVisibility::Hidden,
        }
    }
}

/// Minimum terminal width to allow visible panel.
const MIN_PANEL_WIDTH: u16 = 100;
/// Default panel width as percentage of terminal.
const DEFAULT_PANEL_PCT: u16 = 25;

pub struct TuiLayout {
    pub conversation: Rect,
    pub input: Rect,
    pub status_bar: Rect,
    pub panel: Option<Rect>,       // None when hidden
    pub panel_visibility: PanelVisibility,
}
```

Layout computation:
1. First split vertically: main area (conversation + input + status) vs panel (if visible)
2. Then split main area vertically into conversation, input, status bar
3. When `Hidden`: single column, no horizontal split, zero chrome on conversation edges (AC4.9)
4. When `Visible`: horizontal split — left gets `100 - panel_pct`%, right gets `panel_pct`%
5. When `Expanded`: single column showing only panel content
6. Auto-hide: if `area.width < MIN_PANEL_WIDTH`, force Hidden regardless of requested state (AC4.6)

Panel width is stored as a percentage and adjustable via keybinding:
- `Ctrl+]` → increase panel width by 5%
- `Ctrl+[` → decrease panel width by 5%
- Clamp between 15% and 50%

**Testing:**

- `hidden_layout_no_panel` — panel state Hidden → `panel` is None, conversation full width
- `visible_layout_splits_horizontally` — panel state Visible → both rects have non-zero width
- `expanded_layout_full_panel` — panel state Expanded → conversation/input hidden, panel full width
- `auto_hide_on_narrow_terminal` — terminal width 80 → panel forced Hidden
- `cycle_rotates_states` — Hidden → Visible → Expanded → Hidden
- `zero_chrome_when_hidden` — conversation rect x == 0, width == terminal width (AC4.9)

**Verification:**

Run: `cargo nextest run -p pattern-cli layout`
Expected: all tests pass

**Commit:** `[pattern-cli] panel state and horizontal layout splitting`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: SidePanel widget

**Verifies:** v3-tui.AC4.2, v3-tui.AC4.3, v3-tui.AC4.5

**Files:**
- Create: `crates/pattern_cli/src/tui/panel.rs`
- Modify: `crates/pattern_cli/src/tui/mod.rs`

**Implementation:**

A `StatefulWidget` with switchable content modes and a notification area for Display events.

```rust
/// What the panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelContent {
    #[default]
    Status,
    Thinking,
    Context,
}

pub struct PanelState {
    pub content: PanelContent,
    /// Display::Note messages — rendered in a notification area at the top.
    pub notes: Vec<String>,
    /// Display::Chunk/Final content — rendered in the main panel body.
    pub display_content: String,
    /// Thinking block content shown "in panel" (AC4.5).
    /// Set when user expands a thinking section into the panel.
    pub expanded_thinking: Option<String>,
    /// Max notes to keep before oldest are dropped.
    pub max_notes: usize,
}

pub struct SidePanel;
```

Implements `StatefulWidget` with `type State = PanelState`.

Render layout within the panel rect:
```
┌─ panel ──────────┐
│ [note] agent...  │  ← notification area (1-3 lines, Display::Note)
│──────────────────│
│ [content area]   │  ← switchable: status / thinking / display content
│                  │
│                  │
└──────────────────┘
```

Notification area:
- Shows most recent Display::Note messages (last 3)
- Dimmed style, one line per note
- Auto-scrolls as new notes arrive

Content area based on `PanelContent`:
- **Status**: connection state, active agents list (from periodic `get_status()` polling), last activity timestamp
- **Thinking**: expanded thinking block content (set via AC4.5 — user presses a keybinding on a thinking section to show it in panel instead of inline). Rendered with markdown via `render_markdown()`.
- **Context**: placeholder for now — "context info coming soon". Will show memory snapshot data when memory integration lands.

Display event routing (in `App::handle_turn_event`):
```rust
TurnEvent::Display { kind: DisplayKind::Note, text } => {
    self.panel_state.notes.push(text);
    if self.panel_state.notes.len() > self.panel_state.max_notes {
        self.panel_state.notes.remove(0);
    }
}
TurnEvent::Display { kind: DisplayKind::Chunk, text } => {
    self.panel_state.display_content.push_str(&text);
}
TurnEvent::Display { kind: DisplayKind::Final, text } => {
    self.panel_state.display_content = text;
}
```

**Testing:**

Snapshot tests:
- `panel_renders_notes` — panel with 2 notes in notification area
- `panel_renders_thinking` — expanded thinking content in panel body
- `panel_status_mode` — status view with agent info

Unit tests:
- `note_events_accumulate` — push 5 notes with max 3, only last 3 remain
- `chunk_events_concatenate` — two Chunk events produce combined display_content
- `final_event_replaces` — Final event replaces existing display_content

**Verification:**

Run: `cargo nextest run -p pattern-cli panel`
Expected: all tests pass

**Commit:** `[pattern-cli] SidePanel widget with display event routing`
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_TASK_4 -->
### Task 4: Display event toast popups

**Verifies:** v3-tui.AC4.2, v3-tui.AC4.3

**Files:**
- Create: `crates/pattern_cli/src/tui/toast.rs`
- Modify: `crates/pattern_cli/src/tui/app.rs`

**Implementation:**

When the panel is hidden, Display events show as temporary toast-style popups using tui-popup. Toasts appear in the top-right corner and auto-dismiss after a timeout or on any keypress.

```rust
use std::time::{Duration, Instant};

pub struct Toast {
    pub text: String,
    pub created_at: Instant,
    pub ttl: Duration,
}

pub struct ToastState {
    pub toasts: Vec<Toast>,
}

impl ToastState {
    /// Add a toast. Keeps at most 3 visible.
    pub fn push(&mut self, text: String) {
        self.toasts.push(Toast {
            text,
            created_at: Instant::now(),
            ttl: Duration::from_secs(5),
        });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    /// Remove expired toasts.
    pub fn tick(&mut self) {
        self.toasts.retain(|t| t.created_at.elapsed() < t.ttl);
    }

    /// Dismiss all toasts.
    pub fn dismiss(&mut self) {
        self.toasts.clear();
    }
}
```

Render: position each toast as a small styled block in the top-right corner using `tui_popup::Popup` or manual `Rect` calculation + `Clear` + `Paragraph`.

Routing in App:
```rust
// When Display event arrives and panel is Hidden:
if self.layout_state.panel_visibility == PanelVisibility::Hidden {
    match kind {
        DisplayKind::Note => self.toasts.push(text),
        DisplayKind::Chunk | DisplayKind::Final => self.toasts.push(text),
    }
} else {
    // Route to panel (Task 3 logic).
}
```

**Testing:**

- `toast_auto_expires` — push toast, advance time past TTL, tick → empty
- `toast_max_count` — push 5 toasts, only 3 remain
- `dismiss_clears_all` — dismiss → empty
- Snapshot test: toast rendered in top-right corner

**Verification:**

Run: `cargo nextest run -p pattern-cli toast`
Expected: all tests pass

**Commit:** `[pattern-cli] toast popups for display events when panel hidden`
<!-- END_TASK_4 -->

<!-- START_SUBCOMPONENT_B (tasks 5-6) -->
<!-- START_TASK_5 -->
### Task 5: Status bar

**Verifies:** v3-tui.AC4.4

**Files:**
- Create: `crates/pattern_cli/src/tui/status_bar.rs`
- Modify: `crates/pattern_cli/src/tui/app.rs`

**Implementation:**

A simple widget rendering a single line with key information.

```rust
pub struct StatusBarState {
    pub persona_name: String,
    pub agent_count: usize,
    pub context_tokens: Option<u64>,
    pub connected: bool,
}

pub struct StatusBar;
```

Renders as a styled `Line` with segments:
```
@supervisor │ 3 agents │ 45k ctx │ ● connected
```

- Persona name: bold, prefixed with `@`
- Agent count: dimmed
- Context tokens: formatted as `Nk` (thousands), dimmed. Hidden if `None`.
- Connection indicator: green `●` when connected, red `●` when disconnected

The App periodically polls `client.get_status()` (every 5 seconds when connected) to update agent_count. Token usage updates when `StepReply` arrives (via a new field on `TaggedTurnEvent` or periodic status poll).

Panel state indicator appended when panel is not hidden:
```
@supervisor │ 3 agents │ 45k ctx │ ● │ panel: status
```

**Testing:**

- Snapshot test: `status_bar_connected` — full status bar with all segments
- Snapshot test: `status_bar_disconnected` — red dot, no agent count
- `token_formatting` — 45000 → "45k", 1234567 → "1.2M"

**Verification:**

Run: `cargo nextest run -p pattern-cli status_bar`
Expected: all tests pass

**Commit:** `[pattern-cli] status bar with persona, agents, and context usage`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Clipboard and selection mode

**Verifies:** v3-tui.AC4.8, v3-tui.AC4.9

**Files:**
- Create: `crates/pattern_cli/src/tui/clipboard.rs`
- Modify: `crates/pattern_cli/src/tui/app.rs`

**Implementation:**

Two clipboard strategies:
1. **OSC 52** — write `\x1b]52;c;{base64_text}\x07` to stdout. Works over SSH and remote terminals. Primary strategy.
2. **arboard fallback** — use `arboard::Clipboard::new()?.set_text()` for local clipboard access when OSC 52 isn't supported or as a parallel write.

```rust
use base64::Engine;

/// Copy text to clipboard via OSC 52 (terminal) + arboard (system).
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    // OSC 52 — always attempt, most modern terminals support it.
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let osc = format!("\x1b]52;c;{b64}\x07");
    std::io::Write::write_all(&mut std::io::stdout(), osc.as_bytes())
        .map_err(|e| format!("OSC 52 write failed: {e}"))?;

    // arboard fallback — best effort, don't fail if unavailable.
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.to_string());
    }

    Ok(())
}
```

Selection mode:
- Enter via keybinding (e.g., `Ctrl+S` or a dedicated key)
- In selection mode: mouse drag selects text in the conversation area. Track start/end positions.
- On mouse release: extract selected text from rendered content, call `copy_to_clipboard()`.
- Exit selection mode on Escape or any non-mouse event.

In **Hidden panel state** (AC4.9): conversation area has zero non-text chrome (no borders, no box-drawing characters). Terminal-native text selection with the mouse works automatically — no selection mode needed. This is the default for users who just want to select and copy normally.

Selection mode is primarily useful when the panel is **Visible** — the divider and panel content would otherwise be included in terminal-native selection.

**Testing:**

- `osc52_encodes_correctly` — verify the escape sequence format for known input
- `copy_to_clipboard_doesnt_panic` — smoke test (may not actually copy in CI, but shouldn't crash)
- `hidden_panel_zero_chrome` — verify conversation rect has x=0 and no border characters in buffer (already tested in Task 2, but worth a focused assertion)

**Verification:**

Run: `cargo nextest run -p pattern-cli clipboard`
Expected: tests pass

**Commit:** `[pattern-cli] clipboard support with OSC 52 and arboard fallback`
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_TASK_7 -->
### Task 7: Wire panel into app and integration tests

**Verifies:** v3-tui.AC4.1, v3-tui.AC4.5

**Files:**
- Modify: `crates/pattern_cli/src/tui/app.rs`

**Implementation:**

Wire all Phase 4 components into the app's event loop and render path.

Key routing additions:
- `Ctrl+P` → cycle panel state
- `/panel` command (already registered in Phase 3) → cycle panel state
- When on a collapsed thinking section, a keybinding (e.g., `p` or `Ctrl+E`) → set `panel_state.expanded_thinking` to that section's content, switch panel to Thinking mode (AC4.5). Conversation scroll position unchanged.

Render order update:
1. Compute layout (now with panel awareness)
2. If panel visible: horizontal split, render conversation on left, panel on right
3. If panel expanded: render only panel
4. If panel hidden: render conversation full-width, no chrome
5. Render input area and status bar
6. Render toasts (on top, if any)
7. Render autocomplete popup (on top, if visible)

Display event routing in `handle_turn_event`:
```rust
TurnEvent::Display { kind, text } => {
    if self.layout_state.panel_visibility == PanelVisibility::Hidden {
        self.toasts.push(text);
    } else {
        // Route to panel.
        match kind {
            DisplayKind::Note => self.panel_state.notes.push(text),
            DisplayKind::Chunk => self.panel_state.display_content.push_str(&text),
            DisplayKind::Final => self.panel_state.display_content = text,
        }
    }
}
```

Remove inline Display rendering from conversation model — `SectionKind::Display` variants no longer created in `RenderBatch::push_event`. Display events bypass the conversation entirely.

**Testing:**

Snapshot tests:
- `full_app_with_panel_visible` — conversation + panel + status bar
- `full_app_with_panel_hidden` — conversation only, zero chrome
- `thinking_expanded_in_panel` — thinking content in panel, conversation unchanged
- `display_note_as_toast_when_hidden` — toast popup visible
- `display_note_in_panel_when_visible` — note in panel notification area

**Verification:**

Run: `cargo nextest run -p pattern-cli`
Expected: all tests pass

**Commit:** `[pattern-cli] wire panel, toasts, and display routing into app`
<!-- END_TASK_7 -->
