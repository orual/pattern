# v3 TUI — Phase 2: REPL conversation rendering

**Goal:** A ratatui conversation view that streams markdown, collapses thinking/tool sections, and virtual-scrolls efficiently over long histories.

**Architecture:** The conversation is modelled as `Vec<RenderBatch>` stored inside `ConversationState` (the `StatefulWidget` state). Each batch represents one user message + agent response pair. Batches contain `Section`s (text, thinking, tool call, display) that can be collapsed/expanded. Only batches intersecting the viewport are rendered per frame (virtual scrolling via offset tracking). Markdown is rendered to native ratatui `Text` via `tui-markdown` with syntect code highlighting. Heights are computed lazily during render since `StatefulWidget::render` receives `&mut State`.

**Tech Stack:** ratatui 0.30, tui-markdown 0.3.7, insta (snapshot testing)

**Scope:** Phase 2 of 6 from the v3-tui design plan.

**Codebase verified:** 2026-04-20

---

## Acceptance criteria coverage

This phase implements and tests:

### v3-tui.AC2: Conversation rendering
- **v3-tui.AC2.1 Success:** Agent response streams character-by-character as `TurnEvent::Text` arrives; no buffering delay visible
- **v3-tui.AC2.2 Success:** Markdown in responses renders with syntax highlighting for code blocks, bold/italic for emphasis, proper list formatting
- **v3-tui.AC2.3 Success:** Thinking block renders collapsed as `▸ thinking` by default; clicking/keybinding expands to show full content
- **v3-tui.AC2.4 Success:** Tool call renders collapsed with tool name summary; expandable to show input and output
- **v3-tui.AC2.5 Success:** Expanding a thinking block from 50 turns ago works (any section in history expandable, not just current)
- **v3-tui.AC2.6 Success:** Scrollback through 1000+ messages performs smoothly (virtual scrolling — only visible batches rendered per frame)
- **v3-tui.AC2.7 Success:** ratatui test backend snapshot tests verify conversation rendering for: plain text, markdown with code, collapsed/expanded thinking, tool calls
- **v3-tui.AC2.8 Edge:** Auto-scroll at bottom when new content arrives; scroll position preserved when user scrolls up

---

<!-- START_TASK_1 -->
### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root — add insta and tui-markdown to workspace deps)
- Modify: `crates/pattern_cli/Cargo.toml` (add tui-markdown, insta, pattern-server)

**Step 1: Add workspace dependencies**

In root `Cargo.toml` `[workspace.dependencies]`:
```toml
insta = { version = "1.40", features = ["yaml"] }
tui-markdown = "0.3"
```

**Step 2: Add to pattern_cli**

In `crates/pattern_cli/Cargo.toml` `[dependencies]`:
```toml
tui-markdown = { workspace = true }
pattern-server = { path = "../pattern_server" }
```

Ensure crossterm's async event stream is available. Verify that `ratatui-crossterm` re-exports `crossterm::event::EventStream`, or add `crossterm = { version = "0.28", features = ["event-stream"] }` directly.

In `[dev-dependencies]`:
```toml
insta = { workspace = true }
```

**Step 3: Remove termimad**

Remove `termimad = "0.31"` from pattern_cli deps — tui-markdown replaces it for markdown rendering. Check if termimad is imported anywhere in pattern_cli and remove those imports too.

**Step 4: Verify**

Run: `cargo check -p pattern-cli`
Expected: compiles

**Commit:** `[pattern-cli] add tui-markdown and insta for conversation rendering`
<!-- END_TASK_1 -->

<!-- START_SUBCOMPONENT_A (tasks 2-4) -->
<!-- START_TASK_2 -->
### Task 2: RenderBatch and Section data model

**Files:**
- Create: `crates/pattern_cli/src/tui/mod.rs`
- Create: `crates/pattern_cli/src/tui/model.rs`
- Modify: `crates/pattern_cli/src/main.rs` (add `mod tui;`)

**Implementation:**

The data model for conversation rendering. A `RenderBatch` is one user→agent exchange. Each batch contains ordered `Section`s representing different types of content.

Key design decisions:
- Text and Thinking sections concatenate consecutive same-type events into one section (no per-chunk section proliferation).
- Thinking, ToolCall, ToolResult sections are collapsed by default. Text and Display sections are never collapsed.
- Display events are rendered inline in the conversation for now; Phase 4 will move them to the side panel.
- Height caching uses `Option<u16>` — set to `None` when content changes, computed lazily during render (where `&mut` is available via `StatefulWidget`).
- `ComposedRequest` events are silently ignored (debug-only, not user-facing).

`SectionKind` enum variants:
- `Text(String)` — streamed LLM text
- `Thinking(String)` — LLM reasoning content
- `ToolCall { call_id, function_name, arguments }` — tool invocation
- `ToolResult { call_id, success, content }` — tool result
- `Display { kind: DisplayKind, text }` — agent display output

`Section` struct:
- `kind: SectionKind`
- `collapsed: bool`
- `cached_height: Option<u16>` — invalidated on content change, computed at render width

`RenderBatch` struct:
- `batch_id: SmolStr`
- `user_message: Option<String>`
- `sections: Vec<Section>`
- `streaming: bool`

Methods on `RenderBatch`:
- `push_event(&mut self, event: &TurnEvent)` — append event, extending last section for Text/Thinking, creating new section for others. Sets `Stop` to mark `streaming = false`.
- `compute_heights(&mut self, width: u16)` — walk sections, compute and cache any heights that are `None`. Uses `tui_markdown::from_str()` for Text sections to get line count; plain line count for others.

Methods on `Section`:
- `height(&self) -> u16` — returns cached height (collapsed=1, expanded=cached or 1 as fallback)
- `summary(&self) -> String` — one-line collapsed summary with `▸` prefix and content preview

**Testing:**

- `text_events_concatenate_into_single_section` — two Text events → one section
- `thinking_sections_are_collapsed_by_default` — Thinking section starts collapsed
- `stop_event_marks_batch_not_streaming` — Stop sets `streaming = false`
- `composed_request_not_rendered` — ComposedRequest adds no section
- `display_events_create_sections` — Display events become sections

**Verification:**

Run: `cargo nextest run -p pattern-cli model`
Expected: all tests pass

**Commit:** `[pattern-cli] RenderBatch and Section data model for conversation`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Markdown rendering wrapper

**Files:**
- Create: `crates/pattern_cli/src/tui/markdown.rs`

**Implementation:**

Thin wrapper around `tui-markdown` that converts markdown strings to `ratatui::text::Text` with syntax highlighting.

One function:
```rust
pub fn render_markdown(source: &str) -> Text<'static> {
    tui_markdown::from_str(source)
}
```

For non-markdown content (tool output, thinking blocks), use ratatui's built-in `Paragraph` widget with `Wrap { trim: true }` at render time rather than pre-wrapping text. No custom word-wrap implementation.

Height calculation: use ratatui's `Paragraph::line_count(width)` method (available since ratatui 0.28) for width-aware line counting that accounts for word wrapping. Do NOT use a naive `text.lines.len()` — it gives logical lines, not wrapped lines.

```rust
pub fn markdown_height(source: &str, width: u16) -> u16 {
    use ratatui::widgets::{Paragraph, Wrap};
    let text = tui_markdown::from_str(source);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
    paragraph.line_count(width) as u16
}
```

**Testing:**

- `renders_plain_text` — non-empty output for simple string
- `renders_code_block` — fenced code block produces multiple lines
- `markdown_line_count_matches_lines` — line count agrees with rendered Text

**Verification:**

Run: `cargo nextest run -p pattern-cli markdown`
Expected: tests pass

**Commit:** `[pattern-cli] markdown rendering wrapper over tui-markdown`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: ConversationView widget and state

**Verifies:** v3-tui.AC2.1, v3-tui.AC2.2, v3-tui.AC2.3, v3-tui.AC2.4, v3-tui.AC2.5, v3-tui.AC2.6, v3-tui.AC2.8

**Files:**
- Create: `crates/pattern_cli/src/tui/conversation.rs`

**Implementation:**

A `StatefulWidget` where batches live in the state (not the widget). The widget is a lightweight view; all mutable data is in `ConversationState`.

```rust
#[derive(Debug, Default)]
pub struct ConversationState {
    pub batches: Vec<RenderBatch>,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
    pub focused_section: Option<(usize, usize)>, // (batch_idx, section_idx)
}

pub struct ConversationView;
```

Implements `StatefulWidget` with `type State = ConversationState`.

The `render` method:
1. Call `compute_heights(area.width)` on each batch that has uncached heights (we have `&mut State` here).
2. Calculate total content height.
3. If `auto_scroll`, set `scroll_offset` to show the bottom.
4. Walk batches, accumulating height to find first visible batch.
5. Render visible batches only:
   - User message: rendered as a styled `Line` with `[you]` prefix
   - Text sections: rendered via `render_markdown()` → `Paragraph` with `Wrap`
   - Collapsed sections: single styled `Line` with summary text
   - Expanded thinking/tool sections: full content via `Paragraph` with `Wrap`
6. Streaming indicator: if last batch is `streaming`, show a `▍` cursor or `...` at the end.

Virtual scrolling algorithm:
1. Walk batches from start, accumulating heights until `accumulated >= scroll_offset` → first visible batch, with a line offset within it.
2. Render from first visible batch until viewport filled.
3. Stop when below viewport.

**Testing:**

Snapshot tests using TestBackend + insta:
- `renders_text_batch` — single batch with user message + text response
- `thinking_collapsed_shows_summary` — thinking block shows `▸ thinking:` line
- `thinking_expanded_shows_content` — after toggle, full thinking content visible
- `tool_call_collapsed_shows_name` — tool call shows `▸ tool: function_name`
- `scroll_offset_skips_first_batch` — with offset, first batch not visible
- `auto_scroll_follows_new_content` — new batch pushes viewport to bottom

**Verification:**

Run: `cargo nextest run -p pattern-cli conversation`
Expected: snapshot tests pass (first run creates snapshots, then `cargo insta review` to accept)

**Commit:** `[pattern-cli] ConversationView widget with virtual scrolling`
<!-- END_TASK_4 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 5-6) -->
<!-- START_TASK_5 -->
### Task 5: Scroll and expand/collapse navigation

**Verifies:** v3-tui.AC2.3, v3-tui.AC2.5, v3-tui.AC2.8

**Files:**
- Create: `crates/pattern_cli/src/tui/scroll.rs`

**Implementation:**

Navigation actions for the conversation view. Maps key events to scroll and section toggle actions. Separate from text input handling (Phase 3).

`ConversationAction` enum:
- `ScrollUp(usize)`
- `ScrollDown(usize)`
- `ScrollToBottom`
- `ToggleSection(usize, usize)` — (batch_idx, section_idx)
- `None`

`map_key_to_action(key: KeyEvent, state: &ConversationState) -> ConversationAction`:
- Up → ScrollUp(1)
- Down → ScrollDown(1)
- PageUp → ScrollUp(10)
- PageDown → ScrollDown(10)
- End → ScrollToBottom
- Enter (when focused_section is Some) → ToggleSection
- Tab/Shift+Tab → cycle focused_section through collapsible sections

`apply_action(action, state: &mut ConversationState, viewport_height: u16)`:
- ScrollUp: saturating subtract, set `auto_scroll = false`
- ScrollDown: increase offset, check if at bottom to re-engage `auto_scroll`
- ScrollToBottom: set offset to end, `auto_scroll = true`
- ToggleSection: flip `collapsed`, invalidate batch height cache

**Testing:**

- `scroll_up_decreases_offset`
- `scroll_up_at_zero_stays_at_zero`
- `scroll_up_disables_auto_scroll`
- `scroll_to_bottom_engages_auto_scroll`
- `toggle_section_flips_collapsed`
- `toggle_invalidates_height_cache`

**Verification:**

Run: `cargo nextest run -p pattern-cli scroll`
Expected: tests pass

**Commit:** `[pattern-cli] scroll and expand/collapse navigation`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: TUI layout with conversation + input areas

**Verifies:** v3-tui.AC2.7

**Files:**
- Create: `crates/pattern_cli/src/tui/layout.rs`

**Implementation:**

Layout splitting for the main TUI view: conversation area on top (flexible), input area on bottom (fixed height).

```rust
use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct TuiLayout {
    pub conversation: Rect,
    pub input: Rect,
    pub status_bar: Rect,
}

pub fn compute_layout(area: Rect) -> TuiLayout {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),        // conversation (grows)
            Constraint::Length(3),     // input area (fixed)
            Constraint::Length(1),     // status bar
        ])
        .split(area);

    TuiLayout {
        conversation: chunks[0],
        input: chunks[1],
        status_bar: chunks[2],
    }
}
```

Status bar shows minimal info for now (Phase 4 adds persona and agent count): just "pattern" or a connection status indicator.

**Testing:**

- `layout_allocates_input_area` — input area is 3 rows
- `layout_gives_remaining_to_conversation` — conversation fills the rest
- `layout_handles_small_terminal` — terminal smaller than minimum still produces valid rects

**Verification:**

Run: `cargo nextest run -p pattern-cli layout`
Expected: tests pass

**Commit:** `[pattern-cli] TUI layout splitting`
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_TASK_7 -->
### Task 7: Async event loop

**Verifies:** v3-tui.AC2.1, v3-tui.AC2.8

**Files:**
- Create: `crates/pattern_cli/src/tui/app.rs`

**Implementation:**

The core TUI application struct and event loop. Uses `tokio::select!` to multiplex terminal events and daemon subscription events.

`App` struct:
- `conversation: ConversationState`
- `textarea: TextArea<'static>`
- `should_quit: bool`
- `focus: Focus` enum (`Conversation`, `Input`)

`App::run(&mut self, terminal, event_rx)`:
1. Event loop with periodic UI tick for animations, toast expiry, and status polling.
2. `tokio::select!` on:
   - `crossterm::event::EventStream::next()` — key/resize events
   - `event_rx.recv()` — `TaggedTurnEvent` from daemon subscription (optional, may be `None` if offline)
   - `tokio::time::interval(Duration::from_millis(100)).tick()` — periodic UI refresh (drives toast expiry, streaming cursor blink, status bar updates)
3. On key event: route based on `focus` — if `Input`, handle textarea; if `Conversation`, handle scroll/expand.
4. On `TaggedTurnEvent`: find or create the `RenderBatch` for that `batch_id`, call `push_event()`.
5. On resize: invalidate all height caches (new width).
6. Draw: compute layout, render `ConversationView` + `TextArea` + status bar.

For this phase, the textarea only captures text — no submission or slash commands (Phase 3). Enter in the input area is a no-op. Escape switches focus or quits.

The `event_rx` channel is `Option<mpsc::Receiver<TaggedTurnEvent>>` — `None` means offline/no daemon. The TUI starts with an empty conversation in that case.

**Testing:**

Snapshot test of the full app frame:
- `app_renders_empty_state` — empty conversation + input area + status bar
- `app_renders_with_one_batch` — conversation with content

**Verification:**

Run: `cargo nextest run -p pattern-cli app`
Expected: tests pass

**Commit:** `[pattern-cli] async TUI event loop`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Daemon subscription wiring

**Verifies:** v3-tui.AC2.1

**Files:**
- Modify: `crates/pattern_cli/src/main.rs` (replace `run_tui()` with `App` startup)
- Modify: `crates/pattern_cli/src/tui/app.rs` (add connection logic)

**Implementation:**

Wire the TUI startup to optionally connect to the daemon and subscribe to output.

In `main.rs`, the default (no subcommand) path:
1. Try to connect to daemon via `DaemonClient::connect()`.
2. If connected, subscribe to the default agent's output → get `mpsc::Receiver<TaggedTurnEvent>`.
3. If not connected (daemon not running), start in offline mode with `event_rx = None`.
4. Initialize `App`, run the event loop.

No auto-start of daemon in this phase (Phase 1's Task 9 added `ensure_daemon_running()` — that gets wired in Phase 3 when message submission is added).

```rust
// In run_tui():
let event_rx = match DaemonClient::connect().await {
    Ok(client) => {
        match client.subscribe_output("default".into()).await {
            Ok(rx) => Some(rx),
            Err(_) => None,
        }
    }
    Err(_) => None,
};

let mut app = App::new(event_rx);
app.run(&mut terminal).await?;
```

**Testing:**

- Offline mode tested by running `pattern` with no daemon — should show empty TUI, no crash.
- Connected mode tested manually with daemon running.

**Verification:**

Run: `cargo build -p pattern-cli`
Expected: compiles

Run: `cargo nextest run -p pattern-cli`
Expected: all tests pass (model, markdown, conversation, scroll, layout, app)

**Commit:** `[pattern-cli] wire daemon subscription into TUI startup`
<!-- END_TASK_8 -->
