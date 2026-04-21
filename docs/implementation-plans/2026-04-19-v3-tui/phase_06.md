# v3 TUI — Phase 6: Zellij integration

**Goal:** Auto-session launch, pane spawning for multi-agent views, and graceful standalone fallback when zellij is unavailable.

**Architecture:** The TUI detects zellij state at startup (in-session, available, not available) and adapts behaviour accordingly. When available but not in a session, it auto-launches into a zellij session with a generated KDL layout. When in a session, it provides `/pane` and `/float` commands to spawn additional agent REPLs. The CLI uses `@agent` as the universal syntax for agent targeting. KDL layouts are generated via Askama templates. The daemon tracks connected clients and respects `--stop-daemon-on-exit` only when no other clients remain.

**Tech Stack:** askama 0.15, zellij CLI actions, std::process::Command

**Scope:** Phase 6 of 6 from the v3-tui design plan.

**Codebase verified:** 2026-04-20

---

## Acceptance criteria coverage

This phase implements and tests:

### v3-tui.AC6: Zellij integration
- **v3-tui.AC6.1 Success:** Running `pattern chat` outside zellij (with zellij on PATH) auto-launches a zellij session named `pattern-{project}` and opens the TUI inside it
- **v3-tui.AC6.2 Success:** Inside zellij, `/pane @specialist` spawns `pattern chat @specialist --connect` in a new tiled pane
- **v3-tui.AC6.3 Success:** Inside zellij, `/float @specialist` spawns in a floating pane
- **v3-tui.AC6.4 Success:** Closing one TUI pane doesn't affect other panes or the daemon; agents continue
- **v3-tui.AC6.5 Success:** `zellij attach pattern-{project}` reconnects to existing session with panes intact
- **v3-tui.AC6.6 Success:** Running `pattern chat` without zellij available launches standalone single-pane TUI (no error, full functionality minus multi-pane)
- **v3-tui.AC6.7 Edge:** `--stop-daemon-on-exit` flag: daemon stops when last TUI with this flag disconnects and no other clients are connected

---

<!-- START_TASK_1 -->
### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/pattern_cli/Cargo.toml`

**Step 1: Add workspace deps**

```toml
askama = "0.15"
```

Note: `which` is already a workspace dependency (v8.0). No need to add it.

**Step 2: Add to pattern_cli**

```toml
askama = { workspace = true }
which = { workspace = true }
```

**Step 3: Verify**

Run: `cargo check -p pattern-cli`
Expected: compiles

**Commit:** `[pattern-cli] add askama and which for zellij integration`
<!-- END_TASK_1 -->

<!-- START_SUBCOMPONENT_A (tasks 2-3) -->
<!-- START_TASK_2 -->
### Task 2: Zellij detection

**Verifies:** v3-tui.AC6.6

**Files:**
- Create: `crates/pattern_cli/src/tui/zellij/mod.rs`
- Create: `crates/pattern_cli/src/tui/zellij/detect.rs`
- Modify: `crates/pattern_cli/src/tui/mod.rs` (add `pub mod zellij;`)

**Implementation:**

Detect the three possible zellij states at TUI startup.

```rust
// tui/zellij/detect.rs

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZellijState {
    /// Inside an active zellij session.
    InSession { session_name: String },
    /// Zellij binary is on PATH but we're not in a session.
    Available,
    /// Zellij not found.
    NotAvailable,
}

pub fn detect() -> ZellijState {
    if let Ok(name) = std::env::var("ZELLIJ_SESSION_NAME") {
        return ZellijState::InSession { session_name: name };
    }
    if which::which("zellij").is_ok() {
        ZellijState::Available
    } else {
        ZellijState::NotAvailable
    }
}

/// Derive a session name from the current project.
/// Uses the current directory name, falling back to "default".
pub fn session_name_for_project() -> String {
    let dir_name = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "default".into());
    format!("pattern-{dir_name}")
}
```

`tui/zellij/mod.rs`:
```rust
pub mod detect;
pub mod layout;
pub mod pane;
```

**Testing:**

- `detect_not_available` — with no ZELLIJ env vars and no binary → `NotAvailable`
- `detect_in_session` — set `ZELLIJ_SESSION_NAME=test` → `InSession { session_name: "test" }`
- `session_name_derives_from_dir` — current dir "myproject" → `"pattern-myproject"`

Note: detection tests that check PATH availability may need to be skipped in CI if zellij isn't installed.

**Verification:**

Run: `cargo nextest run -p pattern-cli detect`
Expected: tests pass

**Commit:** `[pattern-cli] zellij environment detection`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: KDL layout generation with Askama

**Verifies:** v3-tui.AC6.1

**Files:**
- Create: `crates/pattern_cli/src/tui/zellij/layout.rs`
- Create: `crates/pattern_cli/templates/zellij_layout.kdl`

**Implementation:**

Askama template for generating zellij KDL layouts. Two layout types: single-pane (default) and multi-agent (constellation).

Template at `templates/zellij_layout.kdl`:
```kdl
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location="zellij:tab-bar"
        }
        children
        pane size=1 borderless=true {
            plugin location="zellij:status-bar"
        }
    }

    tab name="Pattern" focus=true {
{% for pane in panes %}
        pane{% if pane.size_pct %} size="{{ pane.size_pct }}%"{% endif %}{% if pane.name %} name="{{ pane.name }}"{% endif %} {
            command "{{ pane.command }}"
{% for arg in pane.args %}
            args "{{ arg }}"
{% endfor %}
        }
{% endfor %}
    }
}
```

Rust types:
```rust
use askama::Template;

#[derive(Debug, Clone)]
pub struct PaneDef {
    pub name: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub size_pct: Option<u16>,
}

#[derive(Template)]
#[template(path = "zellij_layout.kdl")]
pub struct PatternLayout {
    pub panes: Vec<PaneDef>,
}

impl PatternLayout {
    /// Default single-pane layout for `pattern chat`.
    pub fn single(agent: Option<&str>) -> Self {
        let mut args = vec!["chat".to_string(), "--connect".to_string()];
        if let Some(agent) = agent {
            args.push(format!("@{agent}"));
        }
        Self {
            panes: vec![PaneDef {
                name: agent.map(|a| format!("@{a}")),
                command: "pattern".to_string(),
                args,
                size_pct: None,
            }],
        }
    }

    /// Multi-agent layout — one pane per agent.
    pub fn constellation(agents: &[String]) -> Self {
        let panes = agents.iter().map(|name| PaneDef {
            name: Some(format!("@{name}")),
            command: "pattern".to_string(),
            args: vec!["chat".to_string(), format!("@{name}"), "--connect".to_string()],
            size_pct: None,
        }).collect();
        Self { panes }
    }

    /// Render to a deterministic path and return it.
    /// Uses ~/.pattern/daemon/layout.kdl to avoid tempfile race conditions
    /// (zellij may read the file asynchronously after launch).
    pub fn write_layout(&self) -> std::io::Result<std::path::PathBuf> {
        use std::io::Write;
        let rendered = self.render()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let dir = dirs::home_dir()
            .expect("home directory must exist")
            .join(".pattern")
            .join("daemon");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("layout.kdl");
        std::fs::write(&path, rendered.as_bytes())?;
        Ok(path)
    }
}
```

**Testing:**

- `single_layout_generates_valid_kdl` — render single layout, verify it contains `command "pattern"` and `args "chat" "--connect"`
- `single_layout_with_agent` — render with agent "supervisor", verify `args` includes `"@supervisor"`
- `constellation_layout_multiple_panes` — render with 3 agents, verify 3 pane blocks
- `generated_kdl_is_syntactically_valid` — parse rendered output through the `kdl` crate's parser to verify it produces valid KDL (not just string containment checks)

**Verification:**

Run: `cargo nextest run -p pattern-cli layout`
Expected: tests pass

**Commit:** `[pattern-cli] KDL layout generation via Askama templates`
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_TASK_4 -->
### Task 4: Auto-session launch

**Verifies:** v3-tui.AC6.1, v3-tui.AC6.5

**Files:**
- Modify: `crates/pattern_cli/src/main.rs`
- Create: `crates/pattern_cli/src/tui/zellij/session.rs`

**Implementation:**

When `pattern chat` runs outside zellij with zellij available, auto-launch into a session.

```rust
// tui/zellij/session.rs

use std::process::Command;

use super::detect::{ZellijState, session_name_for_project};
use super::layout::PatternLayout;

/// Launch a zellij session with the Pattern layout.
/// This function execs into zellij — it does not return on success.
pub fn auto_launch_session(agent: Option<&str>) -> miette::Result<()> {
    let session_name = session_name_for_project();
    let layout = PatternLayout::single(agent);
    let layout_path = layout.write_layout()
        .map_err(|e| miette::miette!("failed to write layout: {e}"))?;

    let status = Command::new("zellij")
        .args([
            "attach",
            "--create",
            &session_name,
            "options",
            "--default-layout",
            layout_path.to_str().unwrap(),
        ])
        .status()
        .map_err(|e| miette::miette!("failed to launch zellij: {e}"))?;

    if !status.success() {
        return Err(miette::miette!("zellij exited with status {status}"));
    }

    Ok(())
}
```

In `main.rs`, the chat entry point:
```rust
// Before starting the TUI:
match zellij::detect::detect() {
    ZellijState::Available => {
        // Auto-launch into zellij session.
        zellij::session::auto_launch_session(agent.as_deref())?;
        // If we get here, zellij exited — clean up and return.
        return Ok(());
    }
    ZellijState::InSession { .. } => {
        // Already in zellij — start TUI normally (with --connect).
        // Pane commands available.
    }
    ZellijState::NotAvailable => {
        // No zellij — start TUI standalone.
    }
}
```

Users who don't want auto-session can set `ZELLIJ_AUTO_LAUNCH=0` or pass a flag (e.g., `--no-zellij`).

**Testing:**

- Manual test: run `pattern chat` with zellij installed → zellij session launches
- Manual test: `zellij attach pattern-{project}` reconnects (AC6.5)
- Unit test: `auto_launch_session` constructs correct Command args (mock execution)

**Verification:**

Run: `cargo build -p pattern-cli`
Expected: compiles

**Commit:** `[pattern-cli] auto-launch zellij session on pattern chat`
<!-- END_TASK_4 -->

<!-- START_SUBCOMPONENT_B (tasks 5-6) -->
<!-- START_TASK_5 -->
### Task 5: Pane spawning commands

**Verifies:** v3-tui.AC6.2, v3-tui.AC6.3, v3-tui.AC6.4

**Files:**
- Create: `crates/pattern_cli/src/tui/zellij/pane.rs`
- Modify: `crates/pattern_cli/src/tui/commands.rs` (add pane/float commands)
- Modify: `crates/pattern_cli/src/tui/app.rs` (dispatch pane commands)

**Implementation:**

```rust
// tui/zellij/pane.rs

use std::process::Command;

/// Spawn a new tiled pane running `pattern chat @agent --connect`.
pub fn spawn_tiled(agent: &str) -> Result<(), String> {
    let status = Command::new("zellij")
        .args([
            "action", "new-pane",
            "--name", &format!("@{agent}"),
            "--",
            "pattern", "chat", &format!("@{agent}"), "--connect",
        ])
        .status()
        .map_err(|e| format!("failed to spawn pane: {e}"))?;

    if !status.success() {
        return Err(format!("zellij new-pane exited with {status}"));
    }
    Ok(())
}

/// Spawn a new floating pane running `pattern chat @agent --connect`.
pub fn spawn_floating(agent: &str) -> Result<(), String> {
    let status = Command::new("zellij")
        .args([
            "action", "new-pane",
            "--floating",
            "--name", &format!("@{agent}"),
            "--",
            "pattern", "chat", &format!("@{agent}"), "--connect",
        ])
        .status()
        .map_err(|e| format!("failed to spawn floating pane: {e}"))?;

    if !status.success() {
        return Err(format!("zellij new-pane exited with {status}"));
    }
    Ok(())
}
```

Add to command registry:
```rust
CommandDef { name: "pane",  description: "Open agent in tiled pane",    target: CommandTarget::Local, arg_hint: ArgHint::AgentName },
CommandDef { name: "float", description: "Open agent in floating pane", target: CommandTarget::Local, arg_hint: ArgHint::AgentName },
```

Dispatch in app:
```rust
"pane" => {
    match self.zellij_state {
        ZellijState::InSession { .. } => {
            let agent = args.first()
                .map(|a| a.strip_prefix('@').unwrap_or(a))
                .ok_or("usage: /pane @agent-name")?;
            zellij::pane::spawn_tiled(agent)
                .unwrap_or_else(|e| self.push_system_message(e));
        }
        _ => {
            self.push_system_message("pane commands require zellij".into());
        }
    }
}
"float" => {
    // Same pattern with spawn_floating.
}
```

Each spawned pane runs independently — closing one doesn't affect others or the daemon (AC6.4). The `--connect` flag means it doesn't try to auto-start a new daemon.

**Testing:**

- `pane_command_constructs_correct_args` — verify Command args include agent name and --connect
- `pane_outside_zellij_shows_error` — dispatch /pane when NotAvailable → system message error
- `float_command_adds_floating_flag` — verify --floating in args

**Verification:**

Run: `cargo nextest run -p pattern-cli pane`
Expected: tests pass

**Commit:** `[pattern-cli] /pane and /float commands for zellij pane spawning`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Chat subcommand and CLI flags

**Verifies:** v3-tui.AC6.6, v3-tui.AC6.7

**Files:**
- Modify: `crates/pattern_cli/src/main.rs`

**Implementation:**

Add explicit `Chat` subcommand with positional agent arg and flags. Default (no subcommand) still enters chat.

```rust
#[derive(Subcommand)]
enum Commands {
    /// Manage memory mounts.
    Mount(MountCmd),
    /// Manage messages.db backups.
    Backup(BackupCmd),
    /// Manage the Pattern daemon.
    Daemon(DaemonCmd),
    /// Start a chat session.
    Chat(ChatCmd),
    /// Launch multi-agent constellation view.
    Constellation,
}

#[derive(clap::Args)]
struct ChatCmd {
    /// Agent to talk to (e.g., @supervisor). Defaults to the primary agent.
    #[arg(value_name = "AGENT")]
    agent: Option<String>,

    /// Connect to existing daemon only — don't auto-start.
    #[arg(long)]
    connect: bool,

    /// Stop daemon when this TUI exits (only if no other clients connected).
    #[arg(long)]
    stop_daemon_on_exit: bool,

    /// Skip zellij auto-launch even if available.
    #[arg(long)]
    no_zellij: bool,
}
```

Routing:
```rust
match cli.command {
    Some(Commands::Chat(cmd)) => run_chat(cmd).await?,
    Some(Commands::Constellation) => run_constellation().await?,
    // ... other commands ...
    None => {
        // Default: enter chat mode with no arguments.
        run_chat(ChatCmd::default()).await?;
    }
}
```

The `@` prefix is accepted but optional — `pattern chat @supervisor` and `pattern chat supervisor` both work. Normalization strips the `@` for agent lookup internally.

**`--stop-daemon-on-exit` behaviour:**
- On TUI shutdown, if flag is set: check daemon client count
- If this is the last connected client: send shutdown signal
- If other clients connected: log info, don't shut down
- Daemon tracks connected subscriber count via its actor — incremented on `SubscribeOutput`, decremented when the forwarding task detects client disconnect

**Testing:**

- `chat_subcommand_parses` — `pattern chat @supervisor` → agent = Some("@supervisor")
- `default_enters_chat` — `pattern` with no args → same as `pattern chat`
- `connect_flag_parses` — `pattern chat --connect` → connect = true
- `agent_name_normalized` — `@supervisor` and `supervisor` both resolve to same agent

**Verification:**

Run: `cargo nextest run -p pattern-cli`
Expected: all tests pass

**Commit:** `[pattern-cli] chat subcommand with @agent positional arg and flags`
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_TASK_7 -->
### Task 7: Constellation command (scaffold)

**Verifies:** (scaffolding — no specific AC, prepares for multi-agent)

**Files:**
- Create: `crates/pattern_cli/src/commands/constellation.rs`
- Modify: `crates/pattern_cli/src/commands.rs`

**Implementation:**

```rust
pub async fn run_constellation() -> miette::Result<()> {
    // Connect to daemon.
    let client = DaemonClient::connect().await
        .map_err(|_| miette::miette!("daemon not running — start with `pattern daemon start`"))?;

    // Get agent list.
    let agents = client.list_agents().await
        .map_err(|e| miette::miette!("failed to list agents: {e}"))?;

    if agents.is_empty() {
        println!("no active agents — start a chat first with `pattern chat`");
        return Ok(());
    }

    // Check zellij availability.
    match zellij::detect::detect() {
        ZellijState::NotAvailable => {
            return Err(miette::miette!("constellation view requires zellij"));
        }
        ZellijState::InSession { .. } => {
            // Already in zellij — spawn panes for each agent.
            for agent in &agents {
                zellij::pane::spawn_tiled(&agent.persona_name)
                    .map_err(|e| miette::miette!("failed to spawn pane: {e}"))?;
            }
        }
        ZellijState::Available => {
            // Generate multi-agent layout and launch session.
            let agent_names: Vec<String> = agents.iter()
                .map(|a| a.persona_name.clone())
                .collect();
            let layout = PatternLayout::constellation(&agent_names);
            let layout_path = layout.write_layout()
                .map_err(|e| miette::miette!("layout generation failed: {e}"))?;

            let session_name = format!("{}-constellation",
                zellij::detect::session_name_for_project());

            std::process::Command::new("zellij")
                .args([
                    "attach", "--create", &session_name,
                    "options", "--default-layout",
                    layout_path.to_str().unwrap(),
                ])
                .status()
                .map_err(|e| miette::miette!("zellij launch failed: {e}"))?;
        }
    }

    Ok(())
}
```

**Testing:**

- `constellation_no_agents` — empty agent list → prints message, no error
- `constellation_no_zellij` — NotAvailable → clear error
- `constellation_generates_layout` — 3 agents → layout with 3 panes

**Verification:**

Run: `cargo nextest run -p pattern-cli constellation`
Expected: tests pass

**Commit:** `[pattern-cli] constellation command scaffold`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Standalone fallback and test suite

**Verifies:** v3-tui.AC6.6

**Files:**
- Create: `crates/pattern_cli/tests/zellij_integration.rs`

**Implementation:**

Ensure the full TUI works without zellij:
- Detection returns `NotAvailable` → skip auto-session, start standalone
- `/pane` and `/float` show clear error messages
- All other features (chat, commands, panel, concurrent batches) work normally
- No zellij-related panics or errors in standalone mode

Test suite:
```rust
#[test]
fn standalone_mode_no_errors() {
    // Unset ZELLIJ vars, verify detection returns NotAvailable.
    std::env::remove_var("ZELLIJ_SESSION_NAME");
    std::env::remove_var("ZELLIJ");
    let state = zellij::detect::detect();
    // May be Available if zellij is installed in CI — that's fine.
    // The point is it doesn't crash.
    assert!(matches!(state,
        ZellijState::NotAvailable | ZellijState::Available
    ));
}

#[test]
fn pane_command_outside_zellij_returns_error() {
    // Verify the pane spawning functions return errors when not in zellij.
    // (They check $ZELLIJ_SESSION_NAME internally or the app checks ZellijState.)
}

#[test]
fn kdl_layout_single_is_valid() {
    let layout = PatternLayout::single(Some("supervisor"));
    let rendered = layout.render().unwrap();
    assert!(rendered.contains("pattern"));
    assert!(rendered.contains("@supervisor"));
    assert!(rendered.contains("--connect"));
}

#[test]
fn kdl_layout_constellation_multiple_panes() {
    let layout = PatternLayout::constellation(&[
        "supervisor".into(),
        "researcher".into(),
        "planner".into(),
    ]);
    let rendered = layout.render().unwrap();
    // Three pane blocks.
    assert_eq!(rendered.matches("command").count(), 3);
}
```

Manual test plan (documented in test file comments):
1. `pattern chat` with zellij installed → launches zellij session
2. Inside session: `/pane @test` → new tiled pane appears
3. Inside session: `/float @test` → floating pane appears
4. Close one pane → daemon and other panes unaffected
5. `zellij attach pattern-{project}` → reconnects
6. `pattern chat --no-zellij` → standalone mode even with zellij available
7. `pattern chat` without zellij → standalone, no error

**Verification:**

Run: `cargo nextest run -p pattern-cli zellij`
Expected: unit tests pass. Manual tests documented.

**Commit:** `[pattern-cli] zellij integration tests and standalone fallback`
<!-- END_TASK_8 -->
