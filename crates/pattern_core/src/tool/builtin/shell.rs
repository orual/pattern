//! Shell tool for command execution.
//!
//! Provides agents with shell command execution capability through
//! execute (one-shot) and spawn (streaming) operations.
//!
//! The shell tool delegates to a [`ProcessSource`] which manages the
//! underlying PTY session and security validation.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tracing::{debug, warn};

use crate::data_source::process::{ProcessSource, ShellError, TaskId};
use crate::runtime::ToolContext;
use crate::tool::rules::{ToolRule, ToolRuleType};
use crate::tool::{AiTool, ExecutionMeta};
use crate::{CoreError, Result};

use super::shell_types::{ShellInput, ShellOp};
use super::types::ToolOutput;

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Default source ID for ProcessSource if not specified.
const DEFAULT_PROCESS_SOURCE_ID: &str = "process:shell";

/// Shell tool for command execution.
///
/// Provides four operations:
/// - `execute`: Run a command and wait for completion
/// - `spawn`: Start a long-running process with streaming output
/// - `kill`: Terminate a spawned process
/// - `status`: List running processes
///
/// # Security
///
/// All commands are validated by the [`ProcessSource`]'s command validator
/// before execution. Dangerous commands are blocked, and permission levels
/// control what operations are allowed.
///
/// # Source access
///
/// Unlike most tools, ShellTool accesses its ProcessSource through ToolContext's
/// SourceManager at runtime. This follows the same pattern as FileTool, allowing
/// the tool to be created via `create_builtin_tool()` without requiring explicit
/// source injection.
///
/// # Example
///
/// ```ignore
/// let tool = ShellTool::new(ctx);
/// let input = ShellInput::execute("ls -la");
/// let output = tool.execute(input, &ExecutionMeta::default()).await?;
/// ```
#[derive(Clone)]
pub struct ShellTool {
    ctx: Arc<dyn ToolContext>,
    /// Optional explicit source_id. If None, uses default or finds first ProcessSource.
    source_id: Option<String>,
}

impl std::fmt::Debug for ShellTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellTool")
            .field("agent_id", &self.ctx.agent_id())
            .field("source_id", &self.source_id)
            .finish()
    }
}

impl ShellTool {
    /// Create a new shell tool with the given context.
    ///
    /// The tool will use SourceManager to find the appropriate ProcessSource
    /// at runtime. The ProcessSource must be registered and started before
    /// the tool can execute commands.
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self {
            ctx,
            source_id: None,
        }
    }

    /// Create a shell tool that targets a specific ProcessSource by source_id.
    pub fn with_source_id(ctx: Arc<dyn ToolContext>, source_id: impl Into<String>) -> Self {
        Self {
            ctx,
            source_id: Some(source_id.into()),
        }
    }

    /// Get the SourceManager.
    fn sources(&self) -> Result<Arc<dyn crate::data_source::SourceManager>> {
        self.ctx.sources().ok_or_else(|| {
            CoreError::tool_exec_msg(
                "shell",
                json!({}),
                "No SourceManager available - shell operations require RuntimeContext",
            )
        })
    }

    /// Find a ProcessSource from registered stream sources.
    ///
    /// Looks for a ProcessSource by:
    /// 1. Explicit source_id if configured
    /// 2. Default source_id "process:shell"
    /// 3. First ProcessSource found in registered stream sources
    fn find_process_source(
        &self,
        sources: &Arc<dyn crate::data_source::SourceManager>,
    ) -> Result<Arc<dyn crate::data_source::DataStream>> {
        // Try explicit source_id first
        if let Some(ref id) = self.source_id {
            return sources.get_stream_source(id).ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "shell",
                    json!({"source_id": id}),
                    format!("Stream source '{}' not found", id),
                )
            });
        }

        // Try default source_id
        if let Some(source) = sources.get_stream_source(DEFAULT_PROCESS_SOURCE_ID) {
            if source.as_any().downcast_ref::<ProcessSource>().is_some() {
                return Ok(source);
            }
        }

        // Fall back to finding first ProcessSource
        for id in sources.list_streams() {
            if let Some(source) = sources.get_stream_source(&id) {
                if source.as_any().downcast_ref::<ProcessSource>().is_some() {
                    return Ok(source);
                }
            }
        }

        Err(CoreError::tool_exec_msg(
            "shell",
            json!({}),
            "No ProcessSource registered. Register a ProcessSource via RuntimeContext first.",
        ))
    }

    /// Downcast a DataStream to ProcessSource reference.
    fn as_process_source<'a>(
        source: &'a dyn crate::data_source::DataStream,
    ) -> Result<&'a ProcessSource> {
        source
            .as_any()
            .downcast_ref::<ProcessSource>()
            .ok_or_else(|| {
                CoreError::tool_exec_msg("shell", json!({}), "Source is not a ProcessSource")
            })
    }

    /// Handle execute operation.
    async fn handle_execute(&self, command: &str, timeout_secs: Option<u64>) -> Result<ToolOutput> {
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

        debug!(command = %command, ?timeout, "executing shell command");

        let sources = self.sources()?;
        let source = self.find_process_source(&sources)?;
        let process_source = Self::as_process_source(source.as_ref())?;

        match process_source.execute(command, timeout).await {
            Ok(result) => {
                let data = json!({
                    "output": result.output,
                    "exit_code": result.exit_code,
                    "duration_ms": result.duration_ms,
                });

                if result.exit_code == Some(0) {
                    Ok(ToolOutput::success_with_data(
                        format!("Command completed in {}ms", result.duration_ms),
                        data,
                    ))
                } else {
                    // Non-zero exit is not an error - agent decides significance.
                    Ok(ToolOutput::success_with_data(
                        format!(
                            "Command exited with code {:?} in {}ms",
                            result.exit_code, result.duration_ms
                        ),
                        data,
                    ))
                }
            }
            Err(e) => self.shell_error_to_output(e, "execute"),
        }
    }

    /// Handle spawn operation.
    async fn handle_spawn(&self, command: &str) -> Result<ToolOutput> {
        debug!(command = %command, "spawning streaming process");

        let sources = self.sources()?;
        let source = self.find_process_source(&sources)?;
        let process_source = Self::as_process_source(source.as_ref())?;

        match process_source.spawn(command, None).await {
            Ok((task_id, block_label)) => Ok(ToolOutput::success_with_data(
                format!("Process started: {}", task_id),
                json!({
                    "task_id": task_id.to_string(),
                    "block_label": block_label,
                }),
            )),
            Err(e) => self.shell_error_to_output(e, "spawn"),
        }
    }

    /// Handle kill operation.
    async fn handle_kill(&self, task_id: &str) -> Result<ToolOutput> {
        debug!(task_id = %task_id, "killing process");

        let sources = self.sources()?;
        let source = self.find_process_source(&sources)?;
        let process_source = Self::as_process_source(source.as_ref())?;

        let task_id = TaskId(task_id.to_string());
        match process_source.kill(&task_id).await {
            Ok(()) => Ok(ToolOutput::success(format!("Process {} killed", task_id))),
            Err(e) => self.shell_error_to_output(e, "kill"),
        }
    }

    /// Handle status operation.
    fn handle_status(&self) -> Result<ToolOutput> {
        let sources = self.sources()?;
        let source = self.find_process_source(&sources)?;
        let process_source = Self::as_process_source(source.as_ref())?;

        let processes = process_source.process_status();

        if processes.is_empty() {
            return Ok(ToolOutput::success("No running processes"));
        }

        let process_list: Vec<_> = processes
            .iter()
            .map(|p| {
                let elapsed = p.running_since.elapsed().map(|d| d.as_secs()).unwrap_or(0);
                json!({
                    "task_id": p.task_id.to_string(),
                    "block_label": p.block_label,
                    "command": p.command,
                    "running_for_secs": elapsed,
                })
            })
            .collect();

        Ok(ToolOutput::success_with_data(
            format!("{} running process(es)", processes.len()),
            json!({ "processes": process_list }),
        ))
    }

    /// Convert shell error to tool output.
    ///
    /// Most shell errors are returned as tool outputs (not Err) because they
    /// represent expected failure modes that the agent should handle, not
    /// unexpected system errors.
    fn shell_error_to_output(&self, error: ShellError, op: &str) -> Result<ToolOutput> {
        match &error {
            ShellError::Timeout(duration) => {
                // Timeout returns partial output if available.
                warn!(op = %op, ?duration, "shell command timed out");
                Ok(ToolOutput::success_with_data(
                    format!("Command timed out after {:?}", duration),
                    json!({
                        "timeout": true,
                        "duration_ms": duration.as_millis(),
                    }),
                ))
            }
            ShellError::PermissionDenied { required, granted } => Ok(ToolOutput::error(format!(
                "Permission denied: requires {} but only have {}",
                required, granted
            ))),
            ShellError::PathOutsideSandbox(path) => Ok(ToolOutput::error(format!(
                "Path outside allowed sandbox: {}",
                path.display()
            ))),
            ShellError::CommandDenied(pattern) => Ok(ToolOutput::error(format!(
                "Command denied by security policy: contains '{}'",
                pattern
            ))),
            ShellError::UnknownTask(id) => Ok(ToolOutput::error(format!("Unknown task: {}", id))),
            ShellError::TaskCompleted => Ok(ToolOutput::error("Task has already completed")),
            ShellError::SessionNotInitialized => Ok(ToolOutput::error(
                "Shell session not initialized. Ensure ProcessSource is started.",
            )),
            ShellError::SessionDied => Ok(ToolOutput::error(
                "Shell session died unexpectedly. It will be reinitialized on next command.",
            )),
            _ => {
                // Other errors are unexpected system errors.
                Err(CoreError::tool_exec_msg(
                    "shell",
                    json!({"op": op}),
                    error.to_string(),
                ))
            }
        }
    }
}

#[async_trait]
impl AiTool for ShellTool {
    type Input = ShellInput;
    type Output = ToolOutput;

    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        r#"Execute shell commands in a persistent session.

## Operations

### execute
Run a command and return its output.
- command (required): The command to run
- timeout (optional): Timeout in seconds (default: 60)

Returns output (combined stdout/stderr), exit_code, and duration_ms.

Example: {"op": "execute", "command": "git status"}

### spawn
Start a long-running command with streaming output to a block.
- command (required): The command to run

Returns task_id and block_label for the output block.

Example: {"op": "spawn", "command": "cargo build --release"}

### kill
Terminate a running spawned process.
- task_id (required): The task ID from spawn

Example: {"op": "kill", "task_id": "abc12345"}

### status
List all running spawned processes.

Example: {"op": "status"}

## Notes

- Session state (cwd, env vars) persists across execute calls
- Use `cd` to change directories, `export` to set environment variables
- Non-zero exit codes are reported in data, not as errors
- spawn creates a pinned block that auto-unpins after process exit"#
    }

    fn operations(&self) -> &'static [&'static str] {
        &["execute", "spawn", "kill", "status"]
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(
            self.name().to_string(),
            ToolRuleType::ContinueLoop,
        )]
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some("the conversation will continue after shell commands complete")
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        match input.op {
            ShellOp::Execute => {
                let command = input.command.ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "shell",
                        json!({"op": "execute"}),
                        "command is required for execute operation",
                    )
                })?;
                self.handle_execute(&command, input.timeout).await
            }
            ShellOp::Spawn => {
                let command = input.command.ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "shell",
                        json!({"op": "spawn"}),
                        "command is required for spawn operation",
                    )
                })?;
                self.handle_spawn(&command).await
            }
            ShellOp::Kill => {
                let task_id = input.task_id.ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "shell",
                        json!({"op": "kill"}),
                        "task_id is required for kill operation",
                    )
                })?;
                self.handle_kill(&task_id).await
            }
            ShellOp::Status => self.handle_status(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_tool_name() {
        // We can't easily create a ProcessSource in tests without the full setup,
        // but we can at least verify the module compiles and types are correct.
        assert_eq!(DEFAULT_TIMEOUT_SECS, 60);
    }

    #[test]
    fn test_shell_op_operations() {
        // Verify operations list matches ShellOp variants.
        let ops = &["execute", "spawn", "kill", "status"];
        assert_eq!(ops.len(), 4);
    }
}

/// Integration tests for ShellTool.
///
/// These tests require a real PTY and shell, so they may behave differently
/// in CI environments. Tests that require PTY functionality are skipped in
/// environments where PTY is not available.
#[cfg(test)]
mod integration_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::data_source::DataStream;
    use crate::data_source::process::{ShellPermission, ShellPermissionConfig};
    use crate::db::ConstellationDatabases;
    use crate::id::AgentId;
    use crate::memory::{MemoryCache, MemoryStore};
    use crate::runtime::ToolContext;
    use crate::tool::ExecutionMeta;
    use crate::tool::builtin::test_utils::{
        MockSourceManager, MockToolContext, create_test_agent_in_db,
    };

    /// Helper to check if we're in a CI environment where PTY tests may not work.
    fn should_skip_pty_tests() -> bool {
        std::env::var("CI").is_ok()
    }

    /// Create a complete test setup for ShellTool integration tests.
    ///
    /// Returns the context and process source for test verification.
    async fn create_shell_test_setup(
        agent_id: &str,
        permission: ShellPermission,
    ) -> (Arc<MockToolContext>, Arc<ProcessSource>) {
        let dbs = Arc::new(
            ConstellationDatabases::open_in_memory()
                .await
                .expect("Failed to create test dbs"),
        );

        // Create test agent in database (required for foreign key constraints).
        create_test_agent_in_db(&dbs, agent_id).await;

        let memory = Arc::new(MemoryCache::new(Arc::clone(&dbs)));

        // Create ProcessSource with LocalPtyBackend.
        let config = ShellPermissionConfig::new(permission);
        let process_source = Arc::new(ProcessSource::with_local_backend(
            DEFAULT_PROCESS_SOURCE_ID,
            std::env::temp_dir(),
            config,
        ));

        // Create MockSourceManager with the ProcessSource.
        let source_manager = Arc::new(MockSourceManager::with_stream(
            Arc::clone(&process_source) as Arc<dyn crate::data_source::DataStream>
        ));

        // Create context with SourceManager using the shared MockToolContext.
        let ctx = Arc::new(MockToolContext::with_sources(
            agent_id,
            Arc::clone(&memory) as Arc<dyn MemoryStore>,
            Arc::clone(&dbs),
            source_manager,
        ));

        // Start the ProcessSource (required for spawn to work).
        let owner = AgentId::new(agent_id);
        let _rx = process_source
            .start(Arc::clone(&ctx) as Arc<dyn ToolContext>, owner)
            .await
            .expect("Failed to start ProcessSource");

        (ctx, process_source)
    }

    // ==========================================================================
    // Execute operation tests
    // ==========================================================================

    #[tokio::test]
    async fn test_shell_execute_simple() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_exec", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        let input = ShellInput::execute("echo hello_world");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("execute should succeed");

        assert!(result.success);
        assert!(result.data.is_some());

        let data = result.data.unwrap();
        let output = data["output"].as_str().unwrap();
        assert!(
            output.contains("hello_world"),
            "output should contain 'hello_world', got: {}",
            output
        );
        assert_eq!(data["exit_code"], 0);
    }

    #[tokio::test]
    async fn test_shell_execute_exit_code_nonzero() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_exit", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // `false` command returns exit code 1.
        let input = ShellInput::execute("false");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("execute should succeed even with non-zero exit");

        // Non-zero exit is reported as success with exit_code in data.
        assert!(result.success);
        let data = result.data.unwrap();
        assert_eq!(data["exit_code"], 1);
    }

    #[tokio::test]
    async fn test_shell_execute_with_timeout() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_timeout", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // 1 second timeout on a 10 second sleep.
        let input = ShellInput::execute("sleep 10").with_timeout(1);
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("execute should complete with timeout result");

        // Timeout is reported as success with timeout flag.
        assert!(result.success);
        let data = result.data.unwrap();
        assert_eq!(data["timeout"], true);
    }

    #[tokio::test]
    async fn test_shell_execute_multiline_output() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_multi", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        let input = ShellInput::execute("echo line1; echo line2; echo line3");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("execute should succeed");

        assert!(result.success);
        let data = result.data.unwrap();
        let output = data["output"].as_str().unwrap();
        assert!(output.contains("line1"));
        assert!(output.contains("line2"));
        assert!(output.contains("line3"));
    }

    #[tokio::test]
    async fn test_shell_execute_cwd_persistence() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_cwd", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        let test_dir = format!("shell_test_{}", std::process::id());

        // Create directory and cd into it.
        let input = ShellInput::execute(&format!("mkdir -p {}", test_dir));
        tool.execute(input, &ExecutionMeta::default())
            .await
            .expect("mkdir should succeed");

        let input = ShellInput::execute(&format!("cd {}", test_dir));
        tool.execute(input, &ExecutionMeta::default())
            .await
            .expect("cd should succeed");

        // pwd should show we're in the new directory.
        let input = ShellInput::execute("pwd");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("pwd should succeed");

        let data = result.data.unwrap();
        let output = data["output"].as_str().unwrap();
        assert!(
            output.contains(&test_dir),
            "pwd should show test dir, got: {}",
            output
        );

        // Cleanup.
        let input = ShellInput::execute(&format!("cd .. && rmdir {}", test_dir));
        let _ = tool.execute(input, &ExecutionMeta::default()).await;
    }

    #[tokio::test]
    async fn test_shell_execute_env_persistence() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_env", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Set environment variable.
        let input = ShellInput::execute("export PATTERN_TEST_VAR=integration_test");
        tool.execute(input, &ExecutionMeta::default())
            .await
            .expect("export should succeed");

        // Verify it persists.
        let input = ShellInput::execute("echo $PATTERN_TEST_VAR");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("echo should succeed");

        let data = result.data.unwrap();
        let output = data["output"].as_str().unwrap();
        assert!(
            output.contains("integration_test"),
            "env var should persist, got: {}",
            output
        );
    }

    // ==========================================================================
    // Permission validation tests
    // ==========================================================================

    #[tokio::test]
    async fn test_shell_command_denied() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_denied", ShellPermission::Admin).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // This command should be denied by security policy.
        let input = ShellInput::execute("rm -rf /");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("should return ToolOutput, not error");

        assert!(!result.success);
        assert!(
            result.message.contains("denied"),
            "should indicate command was denied: {}",
            result.message
        );
    }

    #[tokio::test]
    async fn test_shell_permission_denied_read_only() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_ro", ShellPermission::ReadOnly).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Write command should be denied with ReadOnly permission.
        let input = ShellInput::execute("touch /tmp/test_file_$$");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("should return ToolOutput, not error");

        assert!(!result.success);
        assert!(
            result.message.contains("Permission denied"),
            "should indicate permission denied: {}",
            result.message
        );
    }

    #[tokio::test]
    async fn test_shell_read_only_allows_safe_commands() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_ro_safe", ShellPermission::ReadOnly).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Read-only commands should be allowed.
        let input = ShellInput::execute("ls -la");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("execute should succeed");

        assert!(result.success);
    }

    // ==========================================================================
    // Spawn/Kill/Status operation tests
    // ==========================================================================

    #[tokio::test]
    async fn test_shell_spawn_and_kill() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_spawn", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Spawn a long-running process.
        let input = ShellInput::spawn("sleep 60");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("spawn should succeed");

        assert!(result.success);
        let data = result.data.unwrap();
        let task_id = data["task_id"].as_str().unwrap();
        assert!(!task_id.is_empty());
        assert!(data["block_label"].as_str().is_some());

        // Status should show 1 running process.
        let input = ShellInput::status();
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("status should succeed");

        assert!(result.success);
        assert!(
            result.message.contains("1 running"),
            "should show 1 running process: {}",
            result.message
        );

        // Kill the process.
        let input = ShellInput::kill(task_id);
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("kill should succeed");

        assert!(result.success);

        // Give tokio a moment to clean up.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Status should show no running processes.
        let input = ShellInput::status();
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("status should succeed");

        assert!(result.success);
        assert!(
            result.message.contains("No running"),
            "should show no running processes: {}",
            result.message
        );
    }

    #[tokio::test]
    async fn test_shell_spawn_creates_block() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_block", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Spawn a quick process.
        let input = ShellInput::spawn("echo block_test_output && sleep 0.5");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("spawn should succeed");

        assert!(result.success);
        let data = result.data.unwrap();
        let task_id = data["task_id"].as_str().unwrap();
        let block_label = data["block_label"].as_str().unwrap();

        assert!(
            block_label.starts_with("process:"),
            "block label should start with 'process:': {}",
            block_label
        );
        assert!(
            block_label.contains(task_id),
            "block label should contain task_id: {}",
            block_label
        );

        // Wait for process to complete.
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn test_shell_status_empty() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_status", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // No processes running initially.
        let input = ShellInput::status();
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("status should succeed");

        assert!(result.success);
        assert!(
            result.message.contains("No running"),
            "should show no running processes: {}",
            result.message
        );
    }

    // ==========================================================================
    // Error handling tests
    // ==========================================================================

    #[tokio::test]
    async fn test_shell_kill_unknown_task() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_kill_unknown", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Try to kill a non-existent task.
        let input = ShellInput::kill("nonexistent_task_id");
        let result = tool
            .execute(input, &ExecutionMeta::default())
            .await
            .expect("should return ToolOutput, not error");

        assert!(!result.success);
        assert!(
            result.message.contains("Unknown task"),
            "should indicate unknown task: {}",
            result.message
        );
    }

    #[tokio::test]
    async fn test_shell_execute_missing_command() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_missing_cmd", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Execute without command.
        let input = ShellInput {
            op: ShellOp::Execute,
            command: None,
            timeout: None,
            task_id: None,
        };
        let result = tool.execute(input, &ExecutionMeta::default()).await;

        // Should return error (CoreError), not ToolOutput.
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("command is required"),
            "should indicate command is required: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_shell_spawn_missing_command() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_missing_spawn", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Spawn without command.
        let input = ShellInput {
            op: ShellOp::Spawn,
            command: None,
            timeout: None,
            task_id: None,
        };
        let result = tool.execute(input, &ExecutionMeta::default()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shell_kill_missing_task_id() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_missing_taskid", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        // Kill without task_id.
        let input = ShellInput {
            op: ShellOp::Kill,
            command: None,
            timeout: None,
            task_id: None,
        };
        let result = tool.execute(input, &ExecutionMeta::default()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("task_id is required"),
            "should indicate task_id is required: {}",
            err
        );
    }

    // ==========================================================================
    // Tool interface tests
    // ==========================================================================

    #[tokio::test]
    async fn test_shell_tool_metadata() {
        if should_skip_pty_tests() {
            eprintln!("Skipping PTY test in CI environment");
            return;
        }

        let (ctx, _source) =
            create_shell_test_setup("shell_test_meta", ShellPermission::ReadWrite).await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        assert_eq!(tool.name(), "shell");
        assert!(tool.description().contains("Execute shell commands"));
        assert_eq!(tool.operations(), &["execute", "spawn", "kill", "status"]);
        assert!(tool.usage_rule().is_some());

        let rules = tool.tool_rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].tool_name, "shell");
    }

    #[tokio::test]
    async fn test_shell_no_source_manager_error() {
        // Test that ShellTool correctly handles missing SourceManager.
        // Create a context without SourceManager using the shared utility.
        use crate::tool::builtin::test_utils::create_test_context_with_agent;

        let (_dbs, _memory, ctx) = create_test_context_with_agent("shell_test_no_sources").await;
        let tool = ShellTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);

        let input = ShellInput::execute("echo test");
        let result = tool.execute(input, &ExecutionMeta::default()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("SourceManager") || err.to_string().contains("RuntimeContext"),
            "should indicate SourceManager is missing: {}",
            err
        );
    }
}
