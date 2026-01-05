//! ProcessSource - DataStream implementation for shell process management.
//!
//! Provides agents with shell command execution capability through a DataStream
//! interface. Uses a [`ShellBackend`] for actual execution and a [`CommandValidator`]
//! for security policy enforcement.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::data_source::helpers::BlockBuilder;
use crate::data_source::stream::{DataStream, StreamStatus};
use crate::data_source::types::BlockSchemaSpec;
use crate::data_source::{BlockEdit, EditFeedback, Notification, StreamCursor};
use crate::error::Result;
use crate::id::AgentId;
use crate::memory::{BlockSchema, BlockType};
use crate::messages::Message;
use crate::runtime::{MessageOrigin, ToolContext};
use crate::utils::get_next_message_position_sync;

use super::backend::{ExecuteResult, OutputChunk, ShellBackend, TaskId};
use super::error::ShellError;
use super::permission::{CommandValidator, ShellPermissionConfig};

/// Default auto-unpin delay after process exit (5 minutes).
const DEFAULT_UNPIN_DELAY: Duration = Duration::from_secs(300);

/// Information about a spawned streaming process.
#[derive(Debug, Clone)]
struct ProcessInfo {
    task_id: TaskId,
    block_label: String,
    command: String,
    started_at: SystemTime,
    #[allow(dead_code)]
    unpin_delay: Duration,
}

/// Status information for a running process.
#[derive(Debug, Clone)]
pub struct ProcessStatus {
    /// Unique identifier for this process.
    pub task_id: TaskId,
    /// Label of the memory block containing output.
    pub block_label: String,
    /// The command being executed.
    pub command: String,
    /// When the process was started.
    pub running_since: SystemTime,
}

/// ProcessSource manages shell process lifecycles and streams output to blocks.
///
/// Implements [`DataStream`] to integrate with Pattern's data source system.
/// Uses a [`ShellBackend`] for actual command execution and a [`CommandValidator`]
/// for security policy enforcement.
///
/// # Process blocks
///
/// When a process is spawned via [`spawn`](Self::spawn), a pinned memory block is
/// created with label format `process:{task_id}`. This block receives streaming
/// output and is automatically unpinned after a configurable delay once the process
/// exits.
///
/// # Security
///
/// All commands are validated against the configured [`CommandValidator`] before
/// execution. Dangerous commands are blocked, and permission levels control what
/// operations are allowed.
pub struct ProcessSource {
    source_id: String,
    name: String,
    backend: Arc<dyn ShellBackend>,
    validator: Arc<dyn CommandValidator>,
    processes: Arc<DashMap<TaskId, ProcessInfo>>,
    status: RwLock<StreamStatus>,
    tx: RwLock<Option<broadcast::Sender<Notification>>>,
    ctx: RwLock<Option<Arc<dyn ToolContext>>>,
    owner: RwLock<Option<AgentId>>,
}

impl std::fmt::Debug for ProcessSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessSource")
            .field("source_id", &self.source_id)
            .field("name", &self.name)
            .field("status", &*self.status.read())
            .field("process_count", &self.processes.len())
            .finish()
    }
}

impl ProcessSource {
    /// Create a new ProcessSource with the given backend and validator.
    pub fn new(
        source_id: impl Into<String>,
        backend: Arc<dyn ShellBackend>,
        validator: Arc<dyn CommandValidator>,
    ) -> Self {
        let source_id = source_id.into();
        Self {
            name: format!("Shell ({})", &source_id),
            source_id,
            backend,
            validator,
            processes: Arc::new(DashMap::new()),
            status: RwLock::new(StreamStatus::Stopped),
            tx: RwLock::new(None),
            ctx: RwLock::new(None),
            owner: RwLock::new(None),
        }
    }

    /// Create with a default validator from configuration.
    pub fn with_config(
        source_id: impl Into<String>,
        backend: Arc<dyn ShellBackend>,
        config: ShellPermissionConfig,
    ) -> Self {
        let validator = Arc::new(config.build_validator());
        Self::new(source_id, backend, validator)
    }

    /// Create with default local PTY backend and configuration.
    ///
    /// Convenience constructor for the common case of local PTY execution.
    pub fn with_local_backend(
        source_id: impl Into<String>,
        cwd: PathBuf,
        config: ShellPermissionConfig,
    ) -> Self {
        use super::local_pty::LocalPtyBackend;
        let backend = Arc::new(LocalPtyBackend::new(cwd));
        Self::with_config(source_id, backend, config)
    }

    /// Execute a one-shot command. Returns result directly, no block created.
    ///
    /// # Security
    ///
    /// The command is validated against the configured [`CommandValidator`] before
    /// execution. Blocked commands return [`ShellError::CommandDenied`], and
    /// permission violations return [`ShellError::PermissionDenied`].
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Command is denied by security policy
    /// - Command times out
    /// - Shell session dies unexpectedly
    /// - I/O errors occur during execution
    pub async fn execute(
        &self,
        command: &str,
        timeout: Duration,
    ) -> std::result::Result<ExecuteResult, ShellError> {
        // Get cwd for validation.
        let cwd = self.backend.cwd().await.unwrap_or_default();

        // Validate command.
        self.validator.validate(command, &cwd)?;

        // Execute via backend.
        self.backend.execute(command, timeout).await
    }

    /// Spawn a streaming process. Creates a block for output.
    ///
    /// Returns the task ID and block label for the output block. The block is
    /// created pinned so it stays in agent context while the process runs.
    /// After the process exits, the block is automatically unpinned after
    /// `unpin_delay` (default: 5 minutes).
    ///
    /// # Security
    ///
    /// The command is validated before execution. See [`execute`](Self::execute)
    /// for validation details.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Command is denied by security policy
    /// - ProcessSource hasn't been started (no owner/context)
    /// - Block creation fails
    /// - Process spawn fails
    pub async fn spawn(
        &self,
        command: &str,
        unpin_delay: Option<Duration>,
    ) -> std::result::Result<(TaskId, String), ShellError> {
        // Get cwd for validation.
        let cwd = self.backend.cwd().await.unwrap_or_default();

        // Validate command.
        self.validator.validate(command, &cwd)?;

        // Get context and owner - required for block creation.
        let ctx = self.ctx.read().clone();
        let owner = self.owner.read().clone();

        let (ctx, owner) = match (ctx, owner) {
            (Some(c), Some(o)) => (c, o),
            _ => {
                return Err(ShellError::SessionNotInitialized);
            }
        };

        // Spawn via backend.
        let (task_id, mut rx) = self.backend.spawn_streaming(command).await?;
        let block_label = format!("process:{task_id}");
        let unpin_delay = unpin_delay.unwrap_or(DEFAULT_UNPIN_DELAY);

        // Create block for output.
        let memory = ctx.memory();
        let owner_str = owner.to_string();

        BlockBuilder::new(memory, owner.clone(), &block_label)
            .description(format!("Output from: {command}"))
            .schema(BlockSchema::text())
            .block_type(BlockType::Log)
            .pinned()
            .build()
            .await
            .map_err(|e| ShellError::PtyError(format!("failed to create block: {e}")))?;

        // Track process.
        self.processes.insert(
            task_id.clone(),
            ProcessInfo {
                task_id: task_id.clone(),
                block_label: block_label.clone(),
                command: command.to_string(),
                started_at: SystemTime::now(),
                unpin_delay,
            },
        );

        // Spawn task to stream output to block and emit notifications.
        let processes = Arc::clone(&self.processes);
        let tx = self.tx.read().clone();
        let block_label_clone = block_label.clone();
        let task_id_clone = task_id.clone();
        let command_clone = command.to_string();
        let source_id = self.source_id.clone();
        let ctx = Arc::clone(&ctx);

        tokio::spawn(async move {
            let memory = ctx.memory();
            while let Ok(chunk) = rx.recv().await {
                match chunk {
                    OutputChunk::Output(text) => {
                        // Update block.
                        if let Ok(Some(doc)) =
                            memory.get_block(&owner_str, &block_label_clone).await
                            && let Err(e) = doc.append_text(&text, true)
                        {
                            error!(error = %e, "failed to append to process block");
                        }

                        // Send notification for output chunk.
                        if let Some(ref tx) = tx {
                            let batch_id = get_next_message_position_sync();
                            let summary = if text.len() > 100 {
                                format!("{}... ({} bytes)", &text[..100], text.len())
                            } else {
                                text.clone()
                            };
                            let message_text = format!(
                                "Process output from `{}`:\n```\n{}\n```\nBlock: {}",
                                command_clone, summary, block_label_clone
                            );
                            let mut message = Message::user(message_text);
                            message.batch = Some(batch_id);

                            let origin = MessageOrigin::DataSource {
                                source_id: source_id.clone(),
                                source_type: "process".to_string(),
                                item_id: Some(task_id_clone.to_string()),
                                cursor: None,
                            };
                            message.metadata.custom =
                                serde_json::to_value(&origin).unwrap_or_default();

                            let notification = Notification::new(message, batch_id);
                            if let Err(e) = tx.send(notification) {
                                debug!(error = %e, "failed to send output notification (no receivers)");
                            }
                        }

                        debug!(
                            task_id = %task_id_clone,
                            bytes = text.len(),
                            "process output chunk"
                        );
                    }
                    OutputChunk::Exit { code, duration_ms } => {
                        info!(
                            task_id = %task_id_clone,
                            exit_code = ?code,
                            duration_ms,
                            "process exited"
                        );

                        // Update block with exit status.
                        if let Ok(Some(doc)) =
                            memory.get_block(&owner_str, &block_label_clone).await
                        {
                            let status_line = format!(
                                "\n--- Process exited with code {code:?} after {duration_ms}ms ---\n"
                            );
                            let _ = doc.append_text(&status_line, true);
                        }

                        // Send notification for process exit.
                        if let Some(ref tx) = tx {
                            let batch_id = get_next_message_position_sync();
                            let exit_status = match code {
                                Some(0) => "successfully".to_string(),
                                Some(c) => format!("with exit code {}", c),
                                None => "without exit code (killed/crashed)".to_string(),
                            };
                            let message_text = format!(
                                "Process `{}` exited {} after {}ms.\nBlock: {}",
                                command_clone, exit_status, duration_ms, block_label_clone
                            );
                            let mut message = Message::user(message_text);
                            message.batch = Some(batch_id);

                            let origin = MessageOrigin::DataSource {
                                source_id: source_id.clone(),
                                source_type: "process".to_string(),
                                item_id: Some(task_id_clone.to_string()),
                                cursor: None,
                            };
                            message.metadata.custom =
                                serde_json::to_value(&origin).unwrap_or_default();

                            let notification = Notification::new(message, batch_id);
                            if let Err(e) = tx.send(notification) {
                                debug!(error = %e, "failed to send exit notification (no receivers)");
                            }
                        }

                        // Schedule auto-unpin.
                        // Clone ctx to move into nested spawn.
                        let ctx = Arc::clone(&ctx);
                        let owner_str = owner_str.clone();
                        let label = block_label_clone.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(unpin_delay).await;
                            let memory = ctx.memory();
                            if let Err(e) = memory.set_block_pinned(&owner_str, &label, false).await
                            {
                                debug!(error = %e, label = %label, "failed to auto-unpin process block");
                            } else {
                                debug!(label = %label, "auto-unpinned process block");
                            }
                        });

                        processes.remove(&task_id_clone);
                        break;
                    }
                }
            }
        });

        Ok((task_id, block_label))
    }

    /// Kill a running process.
    ///
    /// # Errors
    ///
    /// Returns [`ShellError::UnknownTask`] if no process with the given ID exists.
    pub async fn kill(&self, task_id: &TaskId) -> std::result::Result<(), ShellError> {
        self.backend.kill(task_id).await?;
        self.processes.remove(task_id);
        Ok(())
    }

    /// Get status of all running processes.
    pub fn process_status(&self) -> Vec<ProcessStatus> {
        self.processes
            .iter()
            .map(|entry| {
                let info = entry.value();
                ProcessStatus {
                    task_id: info.task_id.clone(),
                    block_label: info.block_label.clone(),
                    command: info.command.clone(),
                    running_since: info.started_at,
                }
            })
            .collect()
    }

    /// Get the current working directory of the shell session.
    pub async fn cwd(&self) -> Option<PathBuf> {
        self.backend.cwd().await
    }
}

#[async_trait]
impl DataStream for ProcessSource {
    fn source_id(&self) -> &str {
        &self.source_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn block_schemas(&self) -> Vec<BlockSchemaSpec> {
        vec![BlockSchemaSpec::pinned(
            "process:{task_id}",
            BlockSchema::text(),
            "Output from shell process execution",
        )]
    }

    async fn start(
        &self,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>> {
        if *self.status.read() == StreamStatus::Running {
            warn!(
                source_id = %self.source_id,
                "ProcessSource already running, returning new receiver"
            );
            // Return a new receiver if we already have a sender.
            if let Some(tx) = self.tx.read().as_ref() {
                return Ok(tx.subscribe());
            }
        }

        let (tx, rx) = broadcast::channel(256);
        *self.tx.write() = Some(tx.clone());
        *self.ctx.write() = Some(ctx.clone());
        *self.owner.write() = Some(owner.clone());
        *self.status.write() = StreamStatus::Running;

        // Spawn routing task to forward notifications to the owner agent.
        let source_id = self.source_id.clone();
        let routing_rx = tx.subscribe();
        let owner_id = owner.0.clone();

        info!(
            source_id = %source_id,
            owner = %owner_id,
            "ProcessSource started, routing notifications to owner"
        );

        tokio::spawn(async move {
            route_notifications(routing_rx, owner_id, source_id, ctx).await;
        });

        Ok(rx)
    }

    async fn stop(&self) -> Result<()> {
        // Kill all running processes.
        let task_ids: Vec<TaskId> = self.processes.iter().map(|e| e.key().clone()).collect();
        for task_id in task_ids {
            if let Err(e) = self.backend.kill(&task_id).await {
                warn!(error = %e, task_id = %task_id, "failed to kill process during stop");
            }
        }
        self.processes.clear();

        *self.tx.write() = None;
        *self.ctx.write() = None;
        *self.owner.write() = None;
        *self.status.write() = StreamStatus::Stopped;

        info!(source_id = %self.source_id, "ProcessSource stopped");
        Ok(())
    }

    fn pause(&self) {
        *self.status.write() = StreamStatus::Paused;
    }

    fn resume(&self) {
        if *self.status.read() == StreamStatus::Paused {
            *self.status.write() = StreamStatus::Running;
        }
    }

    fn status(&self) -> StreamStatus {
        *self.status.read()
    }

    fn supports_pull(&self) -> bool {
        false
    }

    async fn pull(
        &self,
        _limit: usize,
        _cursor: Option<StreamCursor>,
    ) -> Result<Vec<Notification>> {
        Ok(Vec::new())
    }

    async fn handle_block_edit(
        &self,
        _edit: &BlockEdit,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<EditFeedback> {
        // Process blocks are read-only from agent perspective.
        Ok(EditFeedback::Rejected {
            reason: "process output blocks are read-only".to_string(),
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Route notifications from the process source to the owner agent.
///
/// This runs as a background task, forwarding each notification to the
/// owner agent using the router from ToolContext.
async fn route_notifications(
    mut rx: broadcast::Receiver<Notification>,
    owner_id: String,
    source_id: String,
    ctx: Arc<dyn ToolContext>,
) {
    let router = ctx.router();

    loop {
        match rx.recv().await {
            Ok(notification) => {
                let mut message = notification.message;
                message.batch = Some(notification.batch_id);

                // Extract origin from message metadata.
                let origin = message.metadata.custom.as_object().and_then(|obj| {
                    serde_json::from_value::<MessageOrigin>(serde_json::Value::Object(obj.clone()))
                        .ok()
                });

                // Route to the owner agent.
                match router
                    .route_message_to_agent(&owner_id, message, origin)
                    .await
                {
                    Ok(Some(_)) => {
                        debug!(
                            source_id = %source_id,
                            owner = %owner_id,
                            "routed process notification to owner agent"
                        );
                    }
                    Ok(None) => {
                        warn!(
                            source_id = %source_id,
                            owner = %owner_id,
                            "owner agent not found for process notification"
                        );
                    }
                    Err(e) => {
                        warn!(
                            source_id = %source_id,
                            owner = %owner_id,
                            error = %e,
                            "failed to route process notification"
                        );
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(
                    source_id = %source_id,
                    lagged = n,
                    "process notification routing task lagged"
                );
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!(
                    source_id = %source_id,
                    "process notification channel closed, stopping routing"
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_source_creation() {
        use super::super::local_pty::LocalPtyBackend;
        use crate::data_source::process::ShellPermission;

        let backend = Arc::new(LocalPtyBackend::new(std::env::temp_dir()));
        let config = ShellPermissionConfig::new(ShellPermission::ReadWrite);
        let source = ProcessSource::with_config("test", backend, config);

        assert_eq!(source.source_id(), "test");
        assert_eq!(source.name(), "Shell (test)");
        assert_eq!(source.status(), StreamStatus::Stopped);
        assert!(source.process_status().is_empty());
    }

    #[test]
    fn test_block_schema_spec() {
        use super::super::local_pty::LocalPtyBackend;
        use crate::data_source::process::ShellPermission;

        let backend = Arc::new(LocalPtyBackend::new(std::env::temp_dir()));
        let config = ShellPermissionConfig::new(ShellPermission::ReadWrite);
        let source = ProcessSource::with_config("test", backend, config);

        let schemas = source.block_schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].label_pattern, "process:{task_id}");
        assert!(schemas[0].pinned);
    }
}
