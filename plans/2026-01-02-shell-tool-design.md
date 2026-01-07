# Shell tool and ProcessSource design

## Summary

Add shell command execution capability to Pattern agents via two components:

1. **ProcessSource** - `DataStream` impl managing process lifecycles, streaming output to blocks
2. **Shell tool** - builtin tool exposing execute/spawn/kill/status operations

Design priorities: safety first (permission tiers, path sandboxing), real shell behaviour (PTY session), swappable backends (local → container → remote).

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Shell Tool                           │
│  (builtin tool in crates/pattern_core/src/tool/builtin/)   │
│                                                             │
│  Operations:                                                │
│  - execute: one-shot, returns ToolOutput directly           │
│  - spawn: delegates to ProcessSource, returns task handle   │
│  - kill: terminates spawned process                         │
│  - status: lists running processes                          │
└─────────────────────┬───────────────────────────────────────┘
                      │ spawns/manages via
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                     ProcessSource                           │
│  (DataStream impl in crates/pattern_core/src/data_source/) │
│                                                             │
│  - Maintains PTY shell session (cwd/env persist)            │
│  - Manages spawned process lifecycles                       │
│  - Streams stdout/stderr to Loro blocks                     │
│  - Emits Notifications when output arrives                  │
│  - Swappable backend: local PTY, Docker, Bubblewrap         │
└─────────────────────────────────────────────────────────────┘
```

### Execution modes

**One-shot (`execute`):**
- Runs command in PTY session, waits for completion
- Returns stdout/stderr/exit_code directly in ToolOutput
- No block created, no notifications
- Session state (cwd, env) persists across calls

**Streaming (`spawn`):**
- Starts long-running process
- Output streams to pinned Loro block
- Notifications emitted on output chunks and exit
- Block auto-unpins after configurable interval post-exit

## ProcessSource

```rust
pub struct ProcessSource {
    source_id: String,
    ctx: Arc<dyn ToolContext>,

    // Execution backend (swappable)
    backend: Arc<dyn ShellBackend>,

    // Running processes: task_id → ProcessHandle
    processes: DashMap<String, ProcessHandle>,

    // Notification broadcast
    tx: broadcast::Sender<Notification>,
}

struct ProcessHandle {
    task_id: String,
    block_label: String,           // "process:{task_id}"
    started_at: SystemTime,
    command: String,
    unpin_after: Option<Duration>, // auto-unpin delay after exit
}
```

### Shell backend trait

```rust
trait ShellBackend: Send + Sync {
    fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult>;
    fn spawn_streaming(&self, command: &str) -> Result<(TaskId, broadcast::Receiver<OutputChunk>)>;
    fn kill(&self, task_id: &TaskId) -> Result<()>;
}

struct LocalPtyBackend { session: PtySession }
struct DockerBackend { container_id: String, ... }      // future
struct BubblewrapBackend { sandbox_config: ... }        // future
```

### PTY session management

Use `pty` crate to maintain a real shell session:

```rust
struct PtySession {
    pty: PtyMaster,
    shell_pid: u32,
}

const PROMPT_MARKER: &str = "\x1b]pattern\x07"; // OSC escape sequence

impl LocalPtyBackend {
    fn init_session(&mut self) -> Result<()> {
        // Set custom prompt for command completion detection
        write!(self.session.pty, "PS1='{}'\n", PROMPT_MARKER)?;
        Ok(())
    }

    fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult> {
        // Write command to PTY
        write!(self.session.pty, "{}\n", command)?;

        // Read until prompt marker returns
        let output = self.read_until_prompt(timeout)?;

        Ok(output)
    }
}
```

Benefits of PTY approach:
- `cd`, `export`, aliases just work naturally
- Shell maintains its own state
- Can swap backend without changing semantics

## Shell tool

### Input types

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellOp {
    Execute,
    Spawn,
    Kill,
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ShellInput {
    pub op: ShellOp,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,  // seconds, for execute

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,  // for kill
}
```

### Operation behaviours

| Op | Required params | Returns |
|----|-----------------|---------|
| `execute` | `command` | `ToolOutput` with stdout, stderr, exit_code in data |
| `spawn` | `command` | `ToolOutput` with task_id, block_label |
| `kill` | `task_id` | `ToolOutput` success/failure |
| `status` | none | `ToolOutput` with list of running processes |

### Execute response

```json
{
  "stdout": "...",
  "stderr": "...",
  "exit_code": 0,
  "duration_ms": 1234
}
```

Non-zero exit codes are NOT errors - agent decides significance.

## Permission model

### Permission tiers

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShellPermission {
    ReadOnly,   // git status, ls, cat, etc.
    ReadWrite,  // file modifications, git commit
    Admin,      // unrestricted
}

pub struct ShellPermissionConfig {
    pub default: ShellPermission,
    pub rules: Vec<(GlobPattern, ShellPermission)>,
    pub allowed_paths: Vec<PathBuf>,
    pub strict_path_enforcement: bool,
}
```

### Path sandboxing

```rust
fn validate_paths(&self, command: &str, session_cwd: &Path) -> Result<()> {
    for path in extract_paths(command) {
        let resolved = if path.is_absolute() {
            path.canonicalize()?
        } else {
            session_cwd.join(path).canonicalize()?
        };

        if !self.is_within_allowed(&resolved) {
            return Err(ShellError::PathOutsideSandbox(resolved));
        }
    }
    Ok(())
}
```

Enforcement layers:
1. Session cwd clamping - `cd` outside allowed paths rejected
2. Command path scanning - absolute paths outside bounds rejected
3. Relative path resolution - `../../../etc/passwd` canonicalized and checked

Not bulletproof (shell expansion, symlinks) - defense in depth. Container backend is the real sandbox.

### Command classification

Start simple: denylist of dangerous patterns (`rm -rf /`, `sudo`, `chmod`, `mkfs`). Everything else is `ReadWrite`. Iterate later.

## Block structure

For streaming processes, output goes to a Loro block:

```rust
// Block label: "process:{task_id}"
// BlockSchema::Process

struct ProcessBlock {
    // Metadata (set at spawn)
    command: String,
    cwd: String,
    started_at: i64,

    // Streaming content
    stdout: LoroText,
    stderr: LoroText,

    // Completion (set on exit)
    exit_code: Option<i32>,
    ended_at: Option<i64>,
}
```

Separate stdout/stderr fields (not interleaved) - agent can focus on just errors if needed.

### Block lifecycle

- Created as **pinned** on `spawn`
- On process exit, start auto-unpin timer (default: 5 min)
- Agent can manually pin to keep, or unpin early
- Block stays in store for history even when unpinned

### Notifications

```rust
// On output chunk
Notification {
    message: "process abc123: 42 bytes stdout",
    blocks: vec![block_ref],
}

// On exit
Notification {
    message: "process abc123 exited with code 0",
    blocks: vec![block_ref],
}
```

## Error handling

```rust
#[derive(Debug, Error)]
pub enum ShellError {
    #[error("permission denied: requires {required:?}, have {granted:?}")]
    PermissionDenied { required: ShellPermission, granted: ShellPermission },

    #[error("path outside sandbox: {0}")]
    PathOutsideSandbox(PathBuf),

    #[error("command denied by policy: {0}")]
    CommandDenied(String),

    #[error("command timed out after {0:?}")]
    Timeout(Duration),

    #[error("process spawn failed: {0}")]
    SpawnFailed(#[source] std::io::Error),

    #[error("pty error: {0}")]
    PtyError(String),

    #[error("unknown task: {0}")]
    UnknownTask(String),

    #[error("task already completed")]
    TaskCompleted,
}
```

### Error → ToolOutput mapping

| Error | ToolOutput | Rationale |
|-------|------------|-----------|
| PermissionDenied | `error(msg)` | agent should know why |
| PathOutsideSandbox | `error(msg)` | agent should adjust path |
| Timeout | `success_with_data` with partial output | partial output useful |
| SpawnFailed | `error(msg)` | nothing to show |
| UnknownTask | `error(msg)` | stale task_id |

## Future extensibility

### Container backends (v2)

- `DockerBackend` - spawn shell in container, mount allowed_paths
- `BubblewrapBackend` - lightweight linux sandboxing
- Backend selection via config

### Capability-based permissions (v2)

```rust
struct Capabilities {
    fs_read: bool,
    fs_write: bool,
    network: bool,
    process_spawn: bool,
}

impl From<ShellPermission> for Capabilities { ... }
```

Simple tiers map to capability sets internally, exposed for advanced config later.

### Output access op (v2)

Add `output` operation to fetch current block content for running process without waiting for notification.

### Remote connectors (future)

- `LocalProcessSource` (this design)
- `RemoteProcessSource` connects to user's machine via secure channel

## Implementation steps

1. Add `pty` dependency to pattern_core
2. Create `ShellBackend` trait and `LocalPtyBackend`
3. Create `ProcessSource` implementing `DataStream`
4. Add `BlockSchema::Process` variant
5. Create shell tool with execute/spawn/kill/status ops
6. Implement permission checking and path sandboxing
7. Wire up to tool registry
8. Tests: unit tests for permissions/paths, integration tests for execution
