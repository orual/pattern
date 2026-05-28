# v3 TUI test requirements

Maps each acceptance criterion from the v3-tui design to specific tests.

## AC1: Daemon and IRPC service

| ID | Criterion | Type | Test file | Description |
|---|---|---|---|---|
| v3-tui.AC1.1 | `pattern daemon start` starts the daemon; PID file written to `~/.pattern/daemon/`; unix socket created | integration | `crates/pattern_server/src/state.rs` (unit), `crates/pattern_server/src/main.rs` (manual) | `state_roundtrip` verifies serialization; `is_process_alive_returns_false_for_nonexistent` verifies PID check; manual smoke test verifies start writes state file |
| v3-tui.AC1.2 | IRPC test client connects, calls `send_message`, receives `TurnEvent` stream via `subscribe_output` | integration | `crates/pattern_server/tests/integration.rs` | `full_send_subscribe_flow` sends a message and collects tagged events until Stop; verifies Text + Stop received with correct batch_id |
| v3-tui.AC1.3 | `pattern daemon status` reports running state, active agent count, socket path | unit | `crates/pattern_server/src/server.rs` | `get_status_returns_uptime` verifies status RPC returns valid RuntimeStatus; manual smoke test verifies CLI output |
| v3-tui.AC1.4 | `pattern daemon stop` stops daemon, cleans up PID file and socket | manual | `crates/pattern_server/src/main.rs` | Manual: start daemon, run stop, verify state file removed and process exited |
| v3-tui.AC1.5 | Multiple IRPC clients subscribe to the same agent's output simultaneously; all receive events | integration | `crates/pattern_server/tests/integration.rs`, `crates/pattern_server/src/server.rs` | `multiple_subscribers_receive_same_events` subscribes two clients, sends one message, verifies both receive the event; `subscriber_filtering_by_agent` verifies agent-scoped filtering |
| v3-tui.AC1.6 | Connecting to a non-existent daemon returns a clear error with instructions to run `pattern daemon start` | unit | `crates/pattern_server/src/client.rs` | `connect_without_daemon_returns_clear_error` verifies `DaemonClientError::DaemonNotRunning` with instructive message |
| v3-tui.AC1.7 | `pattern chat` with no running daemon auto-starts daemon, then connects | integration | `crates/pattern_cli/src/commands/daemon.rs` | `ensure_daemon_running()` helper tested via build verification; full flow tested manually |

## AC2: Conversation rendering

| ID | Criterion | Type | Test file | Description |
|---|---|---|---|---|
| v3-tui.AC2.1 | Agent response streams character-by-character as `TurnEvent::Text` arrives; no buffering delay visible | snapshot, manual | `crates/pattern_cli/src/tui/conversation.rs`, `crates/pattern_cli/src/tui/app.rs` | `renders_text_batch` snapshot verifies text rendering; `app_renders_with_one_batch` verifies frame output; streaming smoothness requires human verification |
| v3-tui.AC2.2 | Markdown renders with syntax highlighting for code blocks, bold/italic, proper list formatting | snapshot | `crates/pattern_cli/src/tui/markdown.rs` | `renders_plain_text`, `renders_code_block`, `markdown_line_count_matches_lines` verify markdown-to-ratatui conversion |
| v3-tui.AC2.3 | Thinking block renders collapsed as `▸ thinking` by default; expandable to show full content | snapshot | `crates/pattern_cli/src/tui/conversation.rs`, `crates/pattern_cli/src/tui/scroll.rs` | `thinking_collapsed_shows_summary` and `thinking_expanded_shows_content` snapshots; `toggle_section_flips_collapsed` verifies state change |
| v3-tui.AC2.4 | Tool call renders collapsed with tool name summary; expandable to show input and output | snapshot | `crates/pattern_cli/src/tui/conversation.rs` | `tool_call_collapsed_shows_name` snapshot verifies collapsed summary line |
| v3-tui.AC2.5 | Expanding a thinking block from 50 turns ago works (any section in history expandable) | unit | `crates/pattern_cli/src/tui/scroll.rs` | `toggle_section_flips_collapsed` works on any (batch_idx, section_idx); `toggle_invalidates_height_cache` ensures re-render |
| v3-tui.AC2.6 | Scrollback through 1000+ messages performs smoothly (virtual scrolling) | unit, manual | `crates/pattern_cli/src/tui/conversation.rs` | `scroll_offset_skips_first_batch` verifies viewport clipping; performance with 1000+ batches requires human verification |
| v3-tui.AC2.7 | ratatui test backend snapshot tests verify rendering for: plain text, markdown with code, collapsed/expanded thinking, tool calls | snapshot | `crates/pattern_cli/src/tui/conversation.rs` | `renders_text_batch`, `thinking_collapsed_shows_summary`, `thinking_expanded_shows_content`, `tool_call_collapsed_shows_name` |
| v3-tui.AC2.8 | Auto-scroll at bottom when new content arrives; scroll position preserved when user scrolls up | unit | `crates/pattern_cli/src/tui/scroll.rs` | `scroll_up_disables_auto_scroll`, `scroll_to_bottom_engages_auto_scroll`, `auto_scroll_follows_new_content` |

## AC3: Input and slash commands

| ID | Criterion | Type | Test file | Description |
|---|---|---|---|---|
| v3-tui.AC3.1 | Enter submits message; shift/ctrl+enter inserts newline; multi-line input works | unit | `crates/pattern_cli/src/tui/input.rs` | `enter_submits_text`, `shift_enter_inserts_newline`, `empty_enter_does_nothing` |
| v3-tui.AC3.2 | `/agents` returns agent list from daemon; rendered in conversation or panel | unit | `crates/pattern_cli/src/tui/app.rs` | Command dispatch test verifying `/agents` calls `client.list_agents()` and renders result as system message |
| v3-tui.AC3.3 | `/front @agent-name` changes fronting persona; status bar updates; subsequent messages go to new front | unit | `crates/pattern_cli/src/tui/app.rs` | `front_command_updates_current_agent` verifies `current_agent` field updated after dispatch |
| v3-tui.AC3.4 | `/clear` clears conversation view without affecting daemon state | unit | `crates/pattern_cli/src/tui/app.rs` | `clear_command_empties_conversation` verifies batches cleared; daemon not called |
| v3-tui.AC3.5 | `/quit` exits the TUI cleanly without stopping the daemon | unit | `crates/pattern_cli/src/tui/app.rs` | `quit_command_sets_should_quit` verifies flag set; daemon not stopped |
| v3-tui.AC3.6 | Up arrow cycles through previous message inputs | unit | `crates/pattern_cli/src/tui/input.rs` | `history_up_cycles`, `history_down_restores`, `history_stashes_current_input`, `history_max_size` |
| v3-tui.AC3.7 | Unknown slash command shows "unknown command" error inline, doesn't crash | unit | `crates/pattern_cli/src/tui/app.rs` | `unknown_command_shows_error` verifies inline error note, no panic |
| v3-tui.AC3.8 | Plugin-namespaced command `/plugin-name:cmd` forwards to daemon and returns result | unit | `crates/pattern_cli/src/tui/commands.rs`, `crates/pattern_cli/src/tui/app.rs` | `parse_slash_command_namespaced` verifies parsing; `namespaced_command_forwarded` verifies `run_command` called on daemon |

## AC4: Side panel and display

| ID | Criterion | Type | Test file | Description |
|---|---|---|---|---|
| v3-tui.AC4.1 | Panel has three states: hidden, visible, expanded. `/panel` and Ctrl+P cycle states | unit, snapshot | `crates/pattern_cli/src/tui/layout.rs`, `crates/pattern_cli/src/tui/app.rs` | `cycle_rotates_states`, `hidden_layout_no_panel`, `visible_layout_splits_horizontally`, `expanded_layout_full_panel`; `full_app_with_panel_visible` and `full_app_with_panel_hidden` snapshots |
| v3-tui.AC4.2 | `TurnEvent::Display(Note)` renders in the panel's status area, NOT in conversation | unit, snapshot | `crates/pattern_cli/src/tui/panel.rs`, `crates/pattern_cli/src/tui/app.rs` | `note_events_accumulate`; `display_note_in_panel_when_visible` snapshot; `display_note_as_toast_when_hidden` snapshot |
| v3-tui.AC4.3 | `TurnEvent::Display(Chunk/Final)` renders in the panel content area | unit | `crates/pattern_cli/src/tui/panel.rs` | `chunk_events_concatenate`, `final_event_replaces` |
| v3-tui.AC4.4 | Status bar shows: fronting persona name, active agent count, context token usage | snapshot | `crates/pattern_cli/src/tui/status_bar.rs` | `status_bar_connected`, `status_bar_disconnected` snapshots; `token_formatting` unit test |
| v3-tui.AC4.5 | Expanding a thinking block "in panel" shows full content in side panel without changing conversation scroll | snapshot | `crates/pattern_cli/src/tui/app.rs` | `thinking_expanded_in_panel` snapshot verifies panel shows thinking content and conversation scroll position unchanged |
| v3-tui.AC4.6 | Terminal width below threshold auto-hides panel; toggle is no-op until width sufficient | unit | `crates/pattern_cli/src/tui/layout.rs` | `auto_hide_on_narrow_terminal` verifies panel forced Hidden when width < 100 |
| v3-tui.AC4.7 | Panel width resizable via keybinding or drag | manual | -- | Human verification: Ctrl+] and Ctrl+[ adjust panel width; mouse drag if terminal supports it |
| v3-tui.AC4.8 | Selection mode allows mouse drag to select text; copied to clipboard via OSC 52 | unit, manual | `crates/pattern_cli/src/tui/clipboard.rs` | `osc52_encodes_correctly` verifies escape sequence; `copy_to_clipboard_doesnt_panic` smoke test; actual selection + copy requires human verification |
| v3-tui.AC4.9 | In hidden panel state, conversation area has zero non-text chrome on left and right edges | unit, snapshot | `crates/pattern_cli/src/tui/layout.rs`, `crates/pattern_cli/src/tui/app.rs` | `zero_chrome_when_hidden` verifies x=0 and full width; `full_app_with_panel_hidden` snapshot |

## AC5: Concurrent batches

| ID | Criterion | Type | Test file | Description |
|---|---|---|---|---|
| v3-tui.AC5.1 | User sends message while agent is mid-response; input area accepts immediately | unit | `crates/pattern_cli/src/tui/app.rs` | `submit_during_streaming_creates_new_batch`, `input_not_blocked_during_streaming` |
| v3-tui.AC5.2 | Both batch A and batch B responses stream simultaneously in correct positions | integration | `crates/pattern_cli/tests/concurrent_batches.rs` | Two-concurrent-batches test: send A, send B before A completes, verify both receive events in order |
| v3-tui.AC5.3 | Scrolling up during concurrent streaming shows batch A still receiving text | unit, integration | `crates/pattern_cli/src/tui/scroll.rs`, `crates/pattern_cli/tests/concurrent_batches.rs` | `scroll_up_shows_batch_a_streaming`; integration test simulates scroll-up during concurrent streaming |
| v3-tui.AC5.4 | `TurnEvent`s route to correct `RenderBatch` by `BatchId` -- no cross-contamination | unit, integration | `crates/pattern_cli/src/tui/app.rs`, `crates/pattern_cli/tests/concurrent_batches.rs` | `events_route_to_correct_batch`, `no_cross_contamination`; integration test interleaves events for two batches |
| v3-tui.AC5.5 | Cancelling batch A stops its events; batch B continues unaffected | unit | `crates/pattern_cli/src/tui/app.rs` | `cancel_stops_batch_a`, `cancel_doesnt_affect_batch_b`, `cancel_with_no_streaming_batch` |
| v3-tui.AC5.6 | Three concurrent batches all render in correct positions | integration | `crates/pattern_cli/tests/concurrent_batches.rs` | Three-concurrent-batches test: send A, B, C rapidly, verify all three batches receive their tagged events in correct positions |

## AC6: Zellij integration

| ID | Criterion | Type | Test file | Description |
|---|---|---|---|---|
| v3-tui.AC6.1 | Running `pattern chat` outside zellij (with zellij on PATH) auto-launches a zellij session named `pattern-{project}` | unit, manual | `crates/pattern_cli/src/tui/zellij/layout.rs`, `crates/pattern_cli/tests/zellij_integration.rs` | `single_layout_generates_valid_kdl`, `session_name_derives_from_dir`; manual test verifies full auto-launch |
| v3-tui.AC6.2 | Inside zellij, `/pane @specialist` spawns `pattern chat @specialist --connect` in a new tiled pane | unit, manual | `crates/pattern_cli/src/tui/zellij/pane.rs` | `pane_command_constructs_correct_args`; manual test inside zellij session |
| v3-tui.AC6.3 | Inside zellij, `/float @specialist` spawns in a floating pane | unit, manual | `crates/pattern_cli/src/tui/zellij/pane.rs` | `float_command_adds_floating_flag`; manual test inside zellij session |
| v3-tui.AC6.4 | Closing one TUI pane doesn't affect other panes or the daemon; agents continue | manual | -- | Human verification: close one pane, verify others and daemon unaffected |
| v3-tui.AC6.5 | `zellij attach pattern-{project}` reconnects to existing session with panes intact | manual | -- | Human verification: detach and reattach zellij session |
| v3-tui.AC6.6 | Running `pattern chat` without zellij available launches standalone single-pane TUI (no error) | unit | `crates/pattern_cli/tests/zellij_integration.rs` | `standalone_mode_no_errors` verifies detection returns NotAvailable or Available without crash; `pane_command_outside_zellij_returns_error` verifies graceful degradation |
| v3-tui.AC6.7 | `--stop-daemon-on-exit` flag: daemon stops when last TUI with this flag disconnects | unit, manual | `crates/pattern_cli/src/main.rs` | `chat_subcommand_parses` verifies flag parsing; shutdown logic tested manually |

## Human verification

These criteria require human judgment and cannot be fully automated.

| ID | Criterion | Verification approach |
|---|---|---|
| v3-tui.AC2.1 (partial) | "no buffering delay visible" | Run TUI connected to daemon, send message, observe that characters appear incrementally without perceptible chunking. Compare with a naive buffered implementation to confirm the difference is visible. |
| v3-tui.AC2.6 (partial) | "scrollback through 1000+ messages performs smoothly" | Load 1000+ synthetic batches into conversation state, scroll rapidly with Page Up/Down, observe frame rate stays smooth (no visible stutter or lag). Profile if needed. |
| v3-tui.AC4.7 | "panel width resizable via keybinding or drag" | In visible panel state, press Ctrl+] / Ctrl+[ and verify panel width changes. If terminal supports mouse events, verify drag on divider resizes. |
| v3-tui.AC4.8 (partial) | "selection mode allows mouse drag to select text; copied to clipboard via OSC 52" | Enter selection mode via keybinding, drag to select text in conversation, verify clipboard contains selected text. Test in kitty, alacritty, and wezterm. |
| v3-tui.AC5.2 (partial) | "both responses stream simultaneously" | Send two messages rapidly, observe both response areas growing concurrently. Verify the visual experience matches the design intent (batch A above, batch B below). |
| v3-tui.AC6.1 (partial) | "auto-launches a zellij session" | Run `pattern chat` with zellij installed outside a session. Verify zellij session appears with correct name and layout. |
| v3-tui.AC6.2 | "/pane spawns tiled pane" | Inside zellij, run `/pane @specialist`, verify new tiled pane appears running the correct command. |
| v3-tui.AC6.3 | "/float spawns floating pane" | Inside zellij, run `/float @specialist`, verify floating pane appears. |
| v3-tui.AC6.4 | "closing one pane doesn't affect others or daemon" | Open multiple panes, close one, verify daemon status and remaining panes unaffected. |
| v3-tui.AC6.5 | "zellij attach reconnects with panes intact" | Detach from session (Ctrl+O, d), run `zellij attach pattern-{project}`, verify all panes restored. |
| v3-tui.AC6.7 (partial) | "--stop-daemon-on-exit stops daemon when last client disconnects" | Start daemon, open two TUIs with the flag, close one (daemon stays), close the other (daemon stops). Verify via `pattern daemon status`. |
