//! Local PTY-based shell backend.
//!
//! Uses pty-process to maintain a real shell session where cwd, env vars,
//! and aliases persist across command executions.
//!
//! Exit code detection uses a nonce-based wrapper approach to prevent output
//! injection attacks. Each command is wrapped with a unique marker that includes
//! the exit code, making it impossible for command output to fake the exit code.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, trace, warn};
use uuid::Uuid;

use super::backend::{ExecuteResult, OutputChunk, ShellBackend, TaskId};
use super::error::ShellError;

/// OSC escape sequence used as prompt marker for command completion detection.
const PROMPT_MARKER: &str = "\x1b]pattern-done\x07";

/// Timeout for streaming read operations. If no output is received for this
/// duration, the stream is considered stalled.
const STREAMING_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Information about a running streaming process.
struct RunningProcess {
    #[allow(dead_code)]
    tx: broadcast::Sender<OutputChunk>,
    #[allow(dead_code)]
    started_at: Instant,
    /// Handle to abort the reader task.
    abort_handle: tokio::task::AbortHandle,
    /// Channel to signal the task to kill the child process.
    /// When dropped or sent, the task will kill the child before exiting.
    kill_tx: Option<oneshot::Sender<()>>,
}

impl std::fmt::Debug for RunningProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningProcess")
            .field("started_at", &self.started_at)
            .finish_non_exhaustive()
    }
}

/// Local PTY-based shell backend.
///
/// Maintains a persistent shell session via PTY. Commands are written to
/// the PTY and output is read until the prompt marker appears.
#[derive(Debug)]
pub struct LocalPtyBackend {
    /// Shell to spawn (default: /usr/bin/env bash).
    shell: String,
    /// Initial working directory.
    initial_cwd: PathBuf,
    /// Environment variables to set.
    env: HashMap<String, String>,
    /// Whether to load shell rc files (.bashrc, .bash_profile).
    /// Default is false for reliable prompt detection.
    load_rc: bool,
    /// Running streaming processes.
    running: Arc<DashMap<TaskId, RunningProcess>>,
    /// Current session state (lazily initialized).
    session: Mutex<Option<PtySession>>,
    /// Cached current working directory (updated after each command).
    cached_cwd: Mutex<Option<PathBuf>>,
}

/// Active PTY session state.
struct PtySession {
    pty: pty_process::Pty,
    _child: tokio::process::Child,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession").finish_non_exhaustive()
    }
}

impl LocalPtyBackend {
    /// Create a new backend with default shell.
    ///
    /// The shell is determined in order of preference:
    /// 1. The `SHELL` environment variable (if set and the path exists)
    /// 2. `/bin/bash` (if it exists)
    /// 3. `/bin/sh` (fallback)
    // TODO: Make prompt detection robust enough to handle complex PS1/PROMPT_COMMAND
    // setups (e.g. NixOS vte.sh, starship, oh-my-bash) so we can default load_rc to true.
    // Current issue: OSC escapes in PS1 interfere with our OSC-based prompt marker.
    pub fn new(initial_cwd: PathBuf) -> Self {
        Self {
            shell: Self::find_default_shell(),
            initial_cwd,
            env: HashMap::new(),
            load_rc: false,
            running: Arc::new(DashMap::new()),
            session: Mutex::new(None),
            cached_cwd: Mutex::new(None),
        }
    }

    /// Find a suitable default shell.
    ///
    /// This prefers bash because the prompt detection mechanism (PS1) is designed
    /// for bash/sh-compatible shells. Zsh, fish, and other shells have different
    /// prompt handling that may not work correctly.
    fn find_default_shell() -> String {
        // Try to find bash first - our prompt detection is designed for it.
        // Use `command -v bash` to find it in PATH (works on NixOS).
        if let Ok(output) = std::process::Command::new("sh")
            .args(["-c", "command -v bash"])
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() && std::path::Path::new(&path).exists() {
                    return path;
                }
            }
        }

        // Common bash paths.
        for path in ["/bin/bash", "/usr/bin/bash"] {
            if std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }

        // Fallback to sh.
        for path in ["/bin/sh", "/usr/bin/sh"] {
            if std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }

        // Last resort: try SHELL env var (may be zsh which won't work well).
        if let Ok(shell) = std::env::var("SHELL") {
            if std::path::Path::new(&shell).exists() {
                return shell;
            }
        }

        // Really last resort.
        "bash".to_string()
    }

    /// Create with a specific shell.
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    /// Add environment variables.
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Control whether shell rc files (.bashrc, .bash_profile) load on startup.
    ///
    /// By default, rc files are skipped (`--norc --noprofile`) for reliable
    /// prompt detection. Set to `true` to load them if you need aliases,
    /// functions, or custom PATH from your shell config.
    ///
    /// Note: Complex PS1/PROMPT_COMMAND setups (vte.sh, starship, oh-my-bash)
    /// may interfere with prompt marker detection. If commands time out,
    /// try disabling rc loading.
    pub fn with_load_rc(mut self, load: bool) -> Self {
        self.load_rc = load;
        self
    }

    /// Initialize the PTY session if not already done.
    async fn ensure_session(&self) -> Result<(), ShellError> {
        let mut guard = self.session.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        debug!(shell = %self.shell, cwd = ?self.initial_cwd, "initializing PTY session");

        // Create PTY.
        let (pty, pts) = pty_process::open().map_err(|e| ShellError::PtyError(e.to_string()))?;
        pty.resize(pty_process::Size::new(24, 120))
            .map_err(|e| ShellError::PtyError(e.to_string()))?;

        // Spawn shell - Command uses builder pattern, methods consume and return Self.
        let mut cmd = pty_process::Command::new(&self.shell);
        if !self.load_rc {
            // Skip rc files for reliable prompt detection.
            cmd = cmd.args(["--norc", "--noprofile"]);
        }
        cmd = cmd.current_dir(&self.initial_cwd);
        for (k, v) in &self.env {
            cmd = cmd.env(k, v);
        }
        // Non-interactive shell with explicit prompt.
        cmd = cmd.env("PS1", PROMPT_MARKER);
        cmd = cmd.env("PS2", "");

        let child = cmd
            .spawn(pts)
            .map_err(|e| ShellError::PtyError(e.to_string()))?;

        *guard = Some(PtySession { pty, _child: child });

        // Drop guard before async operations.
        drop(guard);

        // Wait for initial prompt.
        self.read_until_prompt(Duration::from_secs(5)).await?;

        debug!("PTY session initialized");
        Ok(())
    }

    /// Read from PTY until prompt marker appears or timeout.
    /// Returns Err(SessionDied) if we get EOF without seeing the prompt marker.
    /// Output is stripped of ANSI escape sequences.
    async fn read_until_prompt(&self, timeout: Duration) -> Result<String, ShellError> {
        let deadline = Instant::now() + timeout;
        let mut output = String::new();

        loop {
            if Instant::now() > deadline {
                return Err(ShellError::Timeout(timeout));
            }

            let remaining = deadline.saturating_duration_since(Instant::now());

            // Read a chunk with timeout.
            let chunk = {
                let mut guard = self.session.lock().await;
                let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;

                let mut buf = [0u8; 4096];
                match tokio::time::timeout(
                    remaining.min(Duration::from_millis(100)),
                    session.pty.read(&mut buf),
                )
                .await
                {
                    Ok(Ok(0)) => {
                        // EOF without prompt marker means session died.
                        return Err(ShellError::SessionDied);
                    }
                    Ok(Ok(n)) => Some(String::from_utf8_lossy(&buf[..n]).to_string()),
                    Ok(Err(e)) => {
                        // EIO (code 5) is returned on Linux when PTY child exits.
                        // Treat it as session died, not a generic I/O error.
                        if e.raw_os_error() == Some(5) {
                            return Err(ShellError::SessionDied);
                        }
                        return Err(ShellError::Io(e));
                    }
                    Err(_) => None, // Timeout on this read, continue loop.
                }
            };

            if let Some(chunk) = chunk {
                trace!(chunk_len = chunk.len(), "read chunk from PTY");
                output.push_str(&chunk);

                // Check for prompt marker.
                if output.contains(PROMPT_MARKER) {
                    // Strip the prompt marker from output.
                    let marker_pos = output.find(PROMPT_MARKER).unwrap();
                    output.truncate(marker_pos);
                    // Strip ANSI escape sequences before returning.
                    return Ok(Self::strip_ansi(&output));
                }
            }
        }
    }

    /// Generate a unique exit code marker that can't be faked by command output.
    pub(crate) fn generate_exit_marker() -> String {
        let nonce = &Uuid::new_v4().to_string()[..8];
        format!("__PATTERN_EXIT_{nonce}__")
    }

    /// Strip ANSI escape sequences from output.
    fn strip_ansi(input: &str) -> String {
        String::from_utf8_lossy(&strip_ansi_escapes::strip(input)).to_string()
    }

    /// Parse exit code from output containing our marker.
    /// Returns (cleaned_output, exit_code).
    pub(crate) fn parse_exit_code(output: &str, marker: &str) -> Result<(String, i32), ShellError> {
        // Find the LAST occurrence of our marker (in case output contains similar text).
        let search_pattern = format!("{marker}:");
        if let Some(marker_pos) = output.rfind(&search_pattern) {
            let before_marker = &output[..marker_pos];
            let after_marker = &output[marker_pos + search_pattern.len()..];

            // Extract exit code (digits until newline or end).
            let exit_code_str: String = after_marker
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();

            let exit_code = exit_code_str
                .parse::<i32>()
                .map_err(|_| ShellError::ExitCodeParseFailed)?;

            // Clean output: everything before the marker, trimmed.
            let cleaned = before_marker.trim_end().to_string();

            Ok((cleaned, exit_code))
        } else {
            Err(ShellError::ExitCodeParseFailed)
        }
    }

    /// Reinitialize session after it died.
    async fn reinitialize_session(&self) -> Result<(), ShellError> {
        {
            let mut guard = self.session.lock().await;
            *guard = None;
        }
        // Clear cached cwd since session died.
        {
            let mut cwd_guard = self.cached_cwd.lock().await;
            *cwd_guard = None;
        }
        self.ensure_session().await
    }

    /// Query the shell for current working directory and cache it.
    async fn refresh_cwd(&self) -> Result<PathBuf, ShellError> {
        // Use a simple pwd command without our nonce wrapper since we parse it differently.
        {
            let mut guard = self.session.lock().await;
            let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;

            let cmd_line = "pwd\n";
            session
                .pty
                .write_all(cmd_line.as_bytes())
                .await
                .map_err(ShellError::Io)?;
        }

        // Read output until prompt.
        let raw_output = self.read_until_prompt(Duration::from_secs(5)).await?;

        // Parse: output is "pwd\n/actual/path\n" (echo of command + result).
        let path_str = raw_output
            .lines()
            .find(|line| line.starts_with('/') && !line.contains("pwd"))
            .unwrap_or_else(|| raw_output.trim());

        let cwd = PathBuf::from(path_str.trim());

        // Cache it.
        {
            let mut cwd_guard = self.cached_cwd.lock().await;
            *cwd_guard = Some(cwd.clone());
        }

        trace!(cwd = ?cwd, "refreshed cached cwd");
        Ok(cwd)
    }
}

#[async_trait::async_trait]
impl ShellBackend for LocalPtyBackend {
    async fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult, ShellError> {
        self.ensure_session().await?;
        let start = Instant::now();

        debug!(command = %command, ?timeout, "executing command");

        // Generate unique marker for exit code detection.
        let exit_marker = Self::generate_exit_marker();

        // Wrap command to capture exit code with our unique marker.
        let wrapped_command = format!("{command}; echo \"{exit_marker}:$?\"");

        // Write wrapped command to PTY.
        {
            let mut guard = self.session.lock().await;
            let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;

            let cmd_line = format!("{wrapped_command}\n");
            session
                .pty
                .write_all(cmd_line.as_bytes())
                .await
                .map_err(ShellError::Io)?;
        }

        // Read output until prompt.
        let raw_output = match self.read_until_prompt(timeout).await {
            Ok(output) => output,
            Err(ShellError::SessionDied) => {
                // Try to reinitialize for next command.
                warn!("shell session died, will reinitialize on next command");
                let _ = self.reinitialize_session().await;
                return Err(ShellError::SessionDied);
            }
            Err(e) => return Err(e),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        // Strip the echoed wrapped command from the start.
        let output_after_echo = raw_output
            .strip_prefix(&wrapped_command)
            .unwrap_or(&raw_output)
            .trim_start_matches('\n')
            .trim_start_matches('\r');

        // Parse exit code from our marker.
        let (output, exit_code) = Self::parse_exit_code(output_after_echo, &exit_marker)?;

        // Refresh cached cwd after each command (cwd may have changed).
        // This is async but we don't want to fail the whole execute if pwd fails.
        if let Err(e) = self.refresh_cwd().await {
            warn!(error = %e, "failed to refresh cwd after command");
        }

        Ok(ExecuteResult {
            output,
            exit_code: Some(exit_code),
            duration_ms,
        })
    }

    async fn spawn_streaming(
        &self,
        command: &str,
    ) -> Result<(TaskId, broadcast::Receiver<OutputChunk>), ShellError> {
        // For streaming, we spawn a new PTY per process (not the persistent session).
        // This gives us clean exit code handling via child.wait().
        let task_id = TaskId::new();
        let (tx, rx) = broadcast::channel(256);
        let (kill_tx, kill_rx) = oneshot::channel::<()>();

        debug!(task_id = %task_id, command = %command, "spawning streaming process");

        let (pty, pts) = pty_process::open().map_err(|e| ShellError::PtyError(e.to_string()))?;
        let mut cmd = pty_process::Command::new(&self.shell);
        cmd = cmd.current_dir(&self.initial_cwd);
        cmd = cmd.args(["-c", command]);
        for (k, v) in &self.env {
            cmd = cmd.env(k, v);
        }

        let mut child = cmd
            .spawn(pts)
            .map_err(|e| ShellError::PtyError(e.to_string()))?;

        let running = Arc::clone(&self.running);
        let tx_clone = tx.clone();
        let task_id_clone = task_id.clone();

        let handle = tokio::spawn(async move {
            let start = Instant::now();
            let mut reader = BufReader::new(pty);
            let mut line = String::new();

            // Convert oneshot receiver to a future we can select on.
            let mut kill_rx = kill_rx;
            let mut killed = false;

            loop {
                line.clear();

                // Use select to handle both read and kill signal.
                tokio::select! {
                    // Check for kill signal.
                    _ = &mut kill_rx => {
                        debug!(task_id = %task_id_clone, "received kill signal");
                        killed = true;
                        break;
                    }
                    // Read with timeout to prevent hanging forever.
                    read_result = tokio::time::timeout(
                        STREAMING_READ_TIMEOUT,
                        reader.read_line(&mut line)
                    ) => {
                        match read_result {
                            Ok(Ok(0)) => break, // EOF.
                            Ok(Ok(_)) => {
                                // Strip ANSI escapes from streaming output.
                                let clean_line = String::from_utf8_lossy(
                                    &strip_ansi_escapes::strip(&line)
                                ).to_string();
                                let _ = tx_clone.send(OutputChunk::Output(clean_line));
                            }
                            Ok(Err(e)) => {
                                warn!(error = %e, "error reading from streaming PTY");
                                break;
                            }
                            Err(_) => {
                                // Timeout - no output for STREAMING_READ_TIMEOUT.
                                warn!(
                                    task_id = %task_id_clone,
                                    "streaming read timeout after {:?}",
                                    STREAMING_READ_TIMEOUT
                                );
                                let _ = tx_clone.send(OutputChunk::Output(
                                    format!("[timeout: no output for {:?}]\n", STREAMING_READ_TIMEOUT)
                                ));
                                break;
                            }
                        }
                    }
                }
            }

            // Kill the child process if we received a kill signal.
            if killed {
                if let Err(e) = child.kill().await {
                    warn!(error = %e, "failed to kill child process");
                }
            }

            // Wait for child to exit - this gives us the real exit code.
            let status = child.wait().await;
            let exit_code = status.ok().and_then(|s| s.code());
            let duration_ms = start.elapsed().as_millis() as u64;

            let _ = tx_clone.send(OutputChunk::Exit {
                code: exit_code,
                duration_ms,
            });

            running.remove(&task_id_clone);
            debug!(task_id = %task_id_clone, ?exit_code, "streaming process completed");
        });

        self.running.insert(
            task_id.clone(),
            RunningProcess {
                tx,
                started_at: Instant::now(),
                abort_handle: handle.abort_handle(),
                kill_tx: Some(kill_tx),
            },
        );

        Ok((task_id, rx))
    }

    async fn kill(&self, task_id: &TaskId) -> Result<(), ShellError> {
        if let Some((_, mut process)) = self.running.remove(task_id) {
            // Send kill signal to the task so it kills the child process.
            // The task will exit naturally after handling the signal.
            if let Some(kill_tx) = process.kill_tx.take() {
                let _ = kill_tx.send(());
            }
            debug!(task_id = %task_id, "sent kill signal to streaming process");
            Ok(())
        } else {
            Err(ShellError::UnknownTask(task_id.to_string()))
        }
    }

    fn running_tasks(&self) -> Vec<TaskId> {
        self.running.iter().map(|r| r.key().clone()).collect()
    }

    async fn cwd(&self) -> Option<PathBuf> {
        // Return cached cwd if available, otherwise initial_cwd.
        let cached = self.cached_cwd.lock().await;
        cached.clone().or_else(|| Some(self.initial_cwd.clone()))
    }
}

impl Drop for LocalPtyBackend {
    fn drop(&mut self) {
        // Kill any running processes by sending kill signals and aborting tasks.
        // Note: We can't await the kill signal being processed in Drop, but
        // sending the signal will cause the task to kill the child on next poll.
        for mut entry in self.running.iter_mut() {
            if let Some(kill_tx) = entry.kill_tx.take() {
                let _ = kill_tx.send(());
            }
            entry.abort_handle.abort();
        }
    }
}
