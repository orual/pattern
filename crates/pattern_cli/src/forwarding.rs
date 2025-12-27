use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use pattern_core::{
    Agent,
    agent::ResponseEvent,
    coordination::groups::{AgentWithMembership, GroupResponseEvent},
    realtime::{AgentEventContext, AgentEventSink, GroupEventContext, GroupEventSink},
};

use crate::output::Output;

/// CLI printer sink for group events (uses existing pretty printers)
pub struct CliGroupPrinterSink {
    output: Output,
    agents: Vec<AgentWithMembership<Arc<dyn Agent>>>,
    source_tag: Option<String>,
}

impl CliGroupPrinterSink {
    pub fn new(
        output: Output,
        agents: Vec<AgentWithMembership<Arc<dyn Agent>>>,
        source_tag: Option<String>,
    ) -> Self {
        Self {
            output,
            agents,
            source_tag,
        }
    }
}

#[async_trait]
impl GroupEventSink for CliGroupPrinterSink {
    async fn on_event(&self, event: GroupResponseEvent, _ctx: GroupEventContext) {
        // Print group response events
        let tag = self.source_tag.as_deref().unwrap_or("Group");
        match &event {
            GroupResponseEvent::Started {
                pattern,
                agent_count,
                ..
            } => {
                self.output.status(&format!(
                    "[{}] Group started: {} pattern, {} agents",
                    tag, pattern, agent_count
                ));
            }
            GroupResponseEvent::AgentStarted {
                agent_name, role, ..
            } => {
                self.output.status(&format!(
                    "[{}] {} ({:?}) processing...",
                    tag, agent_name, role
                ));
            }
            GroupResponseEvent::TextChunk { agent_id, text, .. } => {
                self.output
                    .status(&format!("[{}] {}: {}", tag, agent_id, text));
            }
            GroupResponseEvent::ToolCallStarted { fn_name, .. } => {
                self.output
                    .status(&format!("[{}] Tool call: {}", tag, fn_name));
            }
            GroupResponseEvent::Complete { .. } => {
                self.output
                    .status(&format!("[{}] Group processing complete", tag));
            }
            _ => {
                // Other events logged at debug level
                tracing::debug!("Group event: {:?}", event);
            }
        }
    }
}

/// Simple file sink that appends Debug-formatted events as plain text lines
pub struct FileGroupSink {
    file: Arc<tokio::sync::Mutex<tokio::fs::File>>,
}

impl FileGroupSink {
    pub async fn create(path: PathBuf) -> miette::Result<Self> {
        use tokio::fs::OpenOptions;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .map_err(|e| miette::miette!("Failed to open file sink: {}", e))?;
        Ok(Self {
            file: Arc::new(tokio::sync::Mutex::new(file)),
        })
    }
}

#[async_trait]
impl GroupEventSink for FileGroupSink {
    async fn on_event(&self, event: GroupResponseEvent, _ctx: GroupEventContext) {
        let mut file = self.file.lock().await;
        let _ = tokio::io::AsyncWriteExt::write_all(
            &mut *file,
            format!("{} {:?}\n", Utc::now().to_rfc3339(), event).as_bytes(),
        )
        .await;
        let _ = tokio::io::AsyncWriteExt::flush(&mut *file).await;
    }
}

/// CLI printer sink for agent events
pub struct CliAgentPrinterSink {
    output: Output,
}

impl CliAgentPrinterSink {
    pub fn new(output: Output) -> Self {
        Self { output }
    }
}

#[async_trait]
impl AgentEventSink for CliAgentPrinterSink {
    async fn on_event(&self, event: ResponseEvent, _ctx: AgentEventContext) {
        crate::chat::print_response_event(event, &self.output);
    }
}

/// File sink for agent events
pub struct FileAgentSink {
    file: Arc<tokio::sync::Mutex<tokio::fs::File>>,
}

impl FileAgentSink {
    pub async fn create(path: PathBuf) -> miette::Result<Self> {
        use tokio::fs::OpenOptions;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .map_err(|e| miette::miette!("Failed to open agent file sink: {}", e))?;
        Ok(Self {
            file: Arc::new(tokio::sync::Mutex::new(file)),
        })
    }
}

#[async_trait]
impl AgentEventSink for FileAgentSink {
    async fn on_event(&self, event: ResponseEvent, _ctx: AgentEventContext) {
        let mut file = self.file.lock().await;
        let _ = tokio::io::AsyncWriteExt::write_all(
            &mut *file,
            format!("{} {:?}\n", Utc::now().to_rfc3339(), event).as_bytes(),
        )
        .await;
        let _ = tokio::io::AsyncWriteExt::flush(&mut *file).await;
    }
}

/// Helper: build sinks for Discord path so CLI can mirror output
/// Build group sinks with a given source tag (e.g., "Discord", "CLI", "Jetstream")
pub async fn build_group_sinks(
    output: &Output,
    agents: &Vec<AgentWithMembership<Arc<dyn Agent>>>,
    source_tag: &str,
) -> Vec<Arc<dyn GroupEventSink>> {
    let mut sinks: Vec<Arc<dyn GroupEventSink>> = Vec::new();
    // Include CLI printer for visibility
    sinks.push(Arc::new(CliGroupPrinterSink::new(
        output.clone(),
        agents.clone(),
        Some(source_tag.to_string()),
    )));
    // Optional file sink via env var
    if let Ok(path) = std::env::var("PATTERN_FORWARD_FILE") {
        if !path.trim().is_empty() {
            if let Ok(sink) = FileGroupSink::create(PathBuf::from(path)).await {
                sinks.push(Arc::new(sink));
            } else {
                output.warning("Failed to initialize file forward sink");
            }
        }
    }
    sinks
}

/// Backward-compat helper for Discord path
pub async fn build_discord_group_sinks(
    output: &Output,
    agents: &Vec<AgentWithMembership<Arc<dyn Agent>>>,
) -> Vec<Arc<dyn GroupEventSink>> {
    build_group_sinks(output, agents, "Discord").await
}

/// Build group sinks for CLI-initiated group chat (CLI printer + optional file)
pub async fn build_cli_group_sinks(
    output: &Output,
    agents: &Vec<AgentWithMembership<Arc<dyn Agent>>>,
) -> Vec<Arc<dyn GroupEventSink>> {
    build_group_sinks(output, agents, "CLI").await
}

/// Build group sinks for Jetstream endpoint routing (CLI printer + optional file)
pub async fn build_jetstream_group_sinks(
    output: &Output,
    agents: &Vec<AgentWithMembership<Arc<dyn Agent>>>,
) -> Vec<Arc<dyn GroupEventSink>> {
    build_group_sinks(output, agents, "Jetstream").await
}

/// Build agent sinks for single-agent CLI chat: CLI printer + optional file
pub async fn build_cli_agent_sinks(output: &Output) -> Vec<Arc<dyn AgentEventSink>> {
    let mut sinks: Vec<Arc<dyn AgentEventSink>> = Vec::new();
    sinks.push(Arc::new(CliAgentPrinterSink::new(output.clone())));
    if let Ok(path) = std::env::var("PATTERN_FORWARD_FILE") {
        if !path.trim().is_empty() {
            if let Ok(sink) = FileAgentSink::create(PathBuf::from(path)).await {
                sinks.push(Arc::new(sink));
            }
        }
    }
    sinks
}
