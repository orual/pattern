# CLAUDE.md - pattern_cli

> **CRITICAL WARNING**: DO NOT run ANY CLI commands during development!
> Production agents are running. Any CLI invocation will disrupt active agents.
> Testing must be done offline, using `cargo nextest run -p pattern-cli`.

Last verified: 2026-04-23

Command-line interface for the Pattern ADHD support system. Binary output: `pattern`.

## Subcommands

- `chat [AGENT]` — interactive TUI chat session with a Pattern agent.
- `constellation [AGENT...]` — multi-agent constellation (one zellij pane per agent).
- `mount {init,status}` — manage memory mounts.
- `backup {create,list,restore,info,rotate}` — manage messages.db snapshots.
- `daemon {start,stop,status}` — manage the `pattern-server` daemon process.

There is no `agent`, `group`, `debug`, `export`, `import`, `atproto`, `config`, or `db` subcommand in the current implementation.

## Architecture

### Entry point

Binary entry at `src/main.rs`. Parsed with `clap` derive macros.

### Daemon connection

`pattern chat` connects to a `pattern-server` daemon over IRPC/QUIC (localhost)
via `pattern_server::client::DaemonClient`. The daemon is auto-started if not
already running (`commands/daemon.rs::ensure_daemon_running`).

After connecting, the TUI sends `InitSession` to tell the daemon which project
it is working in, then subscribes to the resolved agent's output stream.

### TUI stack

Built on:
- `ratatui` + `crossterm` — terminal rendering and input.
- `ratatui-textarea` — multi-line input area.
- `tui-popup` — popup overlays.
- `tui-markdown` — markdown rendering in conversation sections.
- `arboard` + OSC52 fallback — clipboard support.

Persona config loaded via `knus` from `persona.kdl`.

### TUI module layout (`src/tui/`)

```
app.rs           # Root state + render + async event loop (App struct)
autocomplete.rs  # Fuzzy autocomplete popup state and widget
commands.rs      # Command registry + parsing (CommandRegistry, builtin_commands)
conversation.rs  # Virtual-scrolling conversation view with markdown + collapsible sections
input.rs         # TextArea wrapper with history + slash command detection (InputHandler)
layout.rs        # Horizontal split sizing, PanelVisibility, compute_layout_with_panel
markdown.rs      # Markdown rendering helpers
model.rs         # RenderBatch, Section, SectionKind data model
mod.rs           # Re-exports public items
panel.rs         # Side panel for display events (PanelState, SidePanel widget)
scroll.rs        # Scroll action helpers (ConversationAction, apply_action)
status_bar.rs    # Persona + agent count + token usage + connection indicator
toast.rs         # Toast popup notifications for panel-hidden mode
test_utils.rs    # Test helpers (buffer_to_string, etc.) — cfg(test) only
zellij/
  detect.rs      # ZellijState detection (in session, not available, etc.)
  layout.rs      # KDL layout generation for auto-launched sessions
  mod.rs         # Re-exports
  pane.rs        # spawn_tiled / spawn_floating helpers
  session.rs     # Session launch and attach helpers
```

### Zellij integration

- On startup outside a zellij session, `pattern chat` auto-launches a zellij
  session with a layout that includes a `pattern-daemon` tab tailing the daemon
  log. The daemon runs detached — exiting zellij does not kill the daemon.
- Inside a zellij session, `pattern chat` opens as a normal pane.
- `/pane @agent` spawns a sibling tiled pane for another agent.
- `/float @agent` spawns a floating pane.
- If `--no-zellij` is passed, all zellij integration is disabled.
- If `--no-auto-launch-zj` is passed, auto-launch is skipped but `/pane` and
  `/float` still work inside an existing session.

## Slash commands

Commands are dispatched through `InputHandler` → `InputAction::SlashCommand` →
`App::dispatch_slash_command` → `dispatch_local_command` or
`dispatch_runtime_command`.

### Local commands (no daemon call)

| Command | Description |
|---------|-------------|
| `/quit` | Exit the TUI |
| `/clear` | Clear conversation view |
| `/panel` | Cycle panel visibility (Hidden → Visible → Expanded → Hidden) |
| `/pane @agent` | Open agent in a new tiled pane (zellij only) |
| `/float @agent` | Open agent in a floating pane (zellij only) |

### Runtime commands (daemon RPC)

| Command | Description | RPC |
|---------|-------------|-----|
| `/agents` | List active agents | `list_agents()` |
| `/status` | Show uptime + agent count | `get_status()` |
| `/shutdown` | Stop the daemon | `shutdown()` |
| `/cancel` | Cancel current in-flight response | `cancel_batch()` |
| `/front [@agent]` | Switch fronting agent (client-side only — see note) | none |

### Deferred / not registered

- `/context` is not registered. Context/memory display is deferred; the status
  bar already shows token usage and dedicated memory inspection is a larger
  design question.

### Plugin-namespaced commands

Commands containing `:` (e.g. `/plugin-name:do-thing`) are forwarded to the
daemon via `run_command`. The plugin system is future work.

### `/front` limitation

`/front` is client-side only — the TUI tracks which agent it's locked to and
sends every message with `Recipient::Direct(agent_id)`. As of v3-multi-agent
Phase 5, the daemon DOES persist a `FrontingSet` (per-mount, in pattern_db) and
exposes `GetFronting` / `SetFronting` / `UpdateRouting` RPCs, but the TUI does
not yet consume them.

The full TUI fronting integration (default outbound to `Recipient::Auto`,
dynamic fronting status bar driven by `WireTurnEvent::FrontingChanged`,
multi-agent attribution rendering, `/agent <id>` one-shot direct override) is
Phase 6 Task 8 — see
`docs/implementation-plans/2026-04-19-v3-multi-agent/phase_06.md`.

## Command dispatch flow

```
Enter key
  → InputHandler::handle_key
  → InputAction::SlashCommand { name, args }
  → App::handle_input_action
  → App::dispatch_command
  → lookup name in CommandRegistry
  → CommandTarget::Local  → dispatch_local_command
  → CommandTarget::Runtime → dispatch_runtime_command
  → name contains ':'     → dispatch_namespaced_command (run_command RPC)
```

## Key bindings

| Key | Focus | Action |
|-----|-------|--------|
| Enter | Input | Submit message (or accept autocomplete) |
| Shift+Enter / Ctrl+Enter | Input | Insert newline |
| Up / Down | Input (single line) | Cycle history |
| Tab | Input | Switch focus to conversation |
| Esc | Conversation | Switch focus back to input |
| Ctrl+C | Any | Quit |
| Ctrl+P | Any | Cycle panel visibility |
| Ctrl+S | Any | Toggle explicit selection mode |
| Alt+] / Alt+[ | Any | Widen / narrow side panel |
| q | Conversation | Quit |
| p | Conversation | Expand focused thinking section into panel |
| Up / Down / PgUp / PgDn | Conversation | Scroll |
| Space | Conversation | Toggle focused collapsible section |

## Testing

```bash
cargo nextest run -p pattern-cli
cargo insta review   # review snapshot diffs after rendering changes
```

Tests in `app.rs` include both sync unit tests and `#[tokio::test]` async
integration tests that use echo-mode `DaemonServer::spawn()` for real dispatch
verification without LLM credentials.

## Development warnings

- **DO NOT run `pattern` or `pattern-server` during development.**
  Production agents may be running. Any invocation will disrupt them.
- Echo mode (`DaemonServer::spawn()`) runs without credentials and is safe
  for tests.
- Do not add blocking calls to the `App::run` loop — spawn tasks instead.
