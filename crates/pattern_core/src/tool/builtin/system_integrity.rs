use crate::{
    error::Result,
    runtime::ToolContext,
    tool::{AiTool, ExecutionMeta},
};
use async_trait::async_trait;
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::{fs::OpenOptions, io::Write, path::PathBuf, process};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SystemIntegrityInput {
    /// Detailed reason for emergency halt
    pub reason: String,

    /// Severity level: critical, catastrophic, unrecoverable
    #[serde(default = "default_severity")]
    pub severity: String,
}

fn default_severity() -> String {
    "critical".to_string()
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SystemIntegrityOutput {
    pub status: String,
    pub halt_id: String,
    pub message: String,
}

#[derive(Clone)]
pub struct SystemIntegrityTool {
    halt_log_path: PathBuf,
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for SystemIntegrityTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemIntegrityTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl SystemIntegrityTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        let halt_log_path = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pattern")
            .join("halts.log");

        // Ensure directory exists
        if let Some(parent) = halt_log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        Self { halt_log_path, ctx }
    }
}

#[async_trait]
impl AiTool for SystemIntegrityTool {
    type Input = SystemIntegrityInput;
    type Output = SystemIntegrityOutput;

    fn name(&self) -> &str {
        "emergency_halt"
    }

    fn description(&self) -> &str {
        "EMERGENCY ONLY: Immediately terminate the process. Use only when system integrity is at risk or unrecoverable errors occur."
    }

    async fn execute(&self, params: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        let reason = params.reason;
        let severity = params.severity;

        let timestamp = Utc::now();
        let halt_id = format!("halt_{}", timestamp.timestamp());

        // Create archival entry for the halt event
        let memory_content = json!({
            "event_type": "emergency_halt",
            "halt_id": &halt_id,
            "timestamp": timestamp.to_rfc3339(),
            "reason": &reason,
            "severity": &severity,
            "agent_id": self.ctx.agent_id(),
        });

        // Store in agent's archival memory
        let archival_content = format!(
            "EMERGENCY HALT: {} - Severity: {} - Reason: {}",
            halt_id, severity, reason
        );

        match self
            .ctx
            .memory()
            .insert_archival(self.ctx.agent_id(), &archival_content, Some(memory_content))
            .await
        {
            Ok(_) => tracing::info!("Halt event stored in archival memory"),
            Err(e) => tracing::error!("Failed to store halt event in memory: {}", e),
        }

        // Write to log file
        let log_entry = format!(
            "[{}] HALT {} - Agent: {} - Severity: {} - Reason: {}\n",
            timestamp.to_rfc3339(),
            halt_id,
            self.ctx.agent_id(),
            severity,
            reason
        );

        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.halt_log_path)
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(log_entry.as_bytes()) {
                    tracing::error!("Failed to write halt log: {}", e);
                }
            }
            Err(e) => {
                tracing::error!("Failed to open halt log file: {}", e);
            }
        }

        // Log the final message
        tracing::error!("EMERGENCY HALT INITIATED: {}", reason);

        // Prepare response
        let response = SystemIntegrityOutput {
            status: "halt_initiated".to_string(),
            halt_id: halt_id.clone(),
            message: format!(
                "Emergency halt initiated. Process will terminate. Reason: {}",
                reason
            ),
        };

        // Spawn task to terminate after response is sent
        tokio::spawn(async {
            // Give a moment for response to be sent and logs to flush
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Terminate the process
            process::exit(1);
        });

        // Return response immediately so agent can send it
        Ok(response)
    }
}
