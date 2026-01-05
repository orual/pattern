//! MCP tool wrapper stub implementation

use async_trait::async_trait;
use pattern_core::tool::{DynamicTool, DynamicToolExample, ExecutionMeta, ToolRule};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio::time::Duration;
use tracing::debug;

use super::{ToolRequest, ToolResponse};

/// Wraps an MCP tool as a Pattern DynamicTool - Stub implementation
#[derive(Debug)]
pub struct McpToolWrapper {
    /// Name of the tool
    pub tool_name: String,
    /// Name of the MCP server this tool comes from
    pub server_name: String,
    /// Tool description
    pub description: String,
    /// Tool parameter schema
    pub parameters_schema: Value,
    /// Channel to send requests to the MCP client service
    request_tx: mpsc::Sender<ToolRequest>,
    /// Channel to receive responses from the MCP client service
    response_rx: broadcast::Sender<ToolResponse>, // Store sender to create receivers
    /// Timeout for tool calls
    timeout_duration: Duration,
}

impl McpToolWrapper {
    /// Create a new MCP tool wrapper
    pub fn new(
        tool_name: String,
        server_name: String,
        description: String,
        parameters_schema: Value,
        request_tx: mpsc::Sender<ToolRequest>,
        response_rx: broadcast::Sender<ToolResponse>,
    ) -> Self {
        Self {
            tool_name,
            server_name,
            description,
            parameters_schema,
            request_tx,
            response_rx,
            timeout_duration: Duration::from_secs(30),
        }
    }

    /// Set custom timeout duration
    pub fn with_timeout(mut self, duration: Duration) -> Self {
        self.timeout_duration = duration;
        self
    }

    /// Generate a fully qualified tool name
    pub fn qualified_name(&self) -> String {
        format!("{}__{}", self.server_name, self.tool_name)
    }
}

impl Clone for McpToolWrapper {
    fn clone(&self) -> Self {
        Self {
            tool_name: self.tool_name.clone(),
            server_name: self.server_name.clone(),
            description: self.description.clone(),
            parameters_schema: self.parameters_schema.clone(),
            request_tx: self.request_tx.clone(),
            response_rx: self.response_rx.clone(),
            timeout_duration: self.timeout_duration,
        }
    }
}

#[async_trait]
impl DynamicTool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters_schema.clone()
    }

    fn output_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "description": "MCP tool response"
        })
    }

    fn examples(&self) -> Vec<DynamicToolExample> {
        vec![]
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![]
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some("MCP tools require external server connection")
    }

    async fn execute(
        &self,
        params: Value,
        _meta: &ExecutionMeta,
    ) -> std::result::Result<Value, pattern_core::CoreError> {
        debug!(
            "MCP tool '{}' execute called with params: {}",
            self.tool_name, params
        );

        // Create request with unique ID
        let request = ToolRequest::new(self.tool_name.clone(), params);
        let request_id = request.id.clone();

        // Subscribe to responses before sending request
        let mut response_receiver = self.response_rx.subscribe();

        // Send request
        if let Err(e) = self.request_tx.send(request).await {
            return Err(pattern_core::CoreError::ToolNotFound {
                tool_name: self.tool_name.clone(),
                available_tools: vec![format!("Failed to send request: {}", e)],
                src: "mcp_channel".to_string(),
                span: (0, 0),
            });
        }

        // Wait for response with timeout
        let timeout_future = tokio::time::sleep(self.timeout_duration);
        tokio::pin!(timeout_future);

        loop {
            tokio::select! {
                _ = &mut timeout_future => {
                    return Err(pattern_core::CoreError::ToolNotFound {
                        tool_name: self.tool_name.clone(),
                        available_tools: vec!["Request timeout".to_string()],
                        src: "mcp_timeout".to_string(),
                        span: (0, 0),
                    });
                }
                response = response_receiver.recv() => {
                    match response {
                        Ok(tool_response) => {
                            if tool_response.request_id == request_id {
                                match tool_response.result {
                                    Ok(value) => return Ok(value),
                                    Err(error) => {
                                        return Err(pattern_core::CoreError::ToolNotFound {
                                            tool_name: self.tool_name.clone(),
                                            available_tools: vec![error],
                                            src: "mcp_tool_error".to_string(),
                                            span: (0, 0),
                                        });
                                    }
                                }
                            }
                            // Continue loop if this response is for a different request
                        }
                        Err(_) => {
                            return Err(pattern_core::CoreError::ToolNotFound {
                                tool_name: self.tool_name.clone(),
                                available_tools: vec!["Response channel closed".to_string()],
                                src: "mcp_channel_closed".to_string(),
                                span: (0, 0),
                            });
                        }
                    }
                }
            }
        }
    }

    fn clone_box(&self) -> Box<dyn DynamicTool> {
        Box::new(self.clone())
    }
}
