// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! MCP client — connects to a server and discovers/calls tools.

use rmcp::{
    service::{DynService, RoleClient, RunningService, ServiceExt},
    transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess},
};
use tokio::process::Command;

use super::config::{McpServerConfig, TransportConfig};

/// Metadata about a tool discovered from an MCP server.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
}

/// A live connection to a single MCP server.
pub struct McpClient {
    /// Server config (for reconnection/diagnostics).
    pub config: McpServerConfig,
    /// The running rmcp service.
    service: RunningService<RoleClient, Box<dyn DynService<RoleClient>>>,
}

impl McpClient {
    /// Connect to an MCP server.
    pub async fn connect(config: McpServerConfig) -> Result<Self, McpConnectError> {
        let service = match &config.transport {
            TransportConfig::Stdio { command, args, env } => {
                let transport = TokioChildProcess::new(Command::new(command).configure(|cmd| {
                    for arg in args {
                        cmd.arg(arg);
                    }
                    for (k, v) in env {
                        cmd.env(k, v);
                    }
                }))
                .map_err(|e| McpConnectError::Transport(format!("stdio spawn: {e}")))?;

                ().into_dyn()
                    .serve(transport)
                    .await
                    .map_err(|e| McpConnectError::Handshake(format!("stdio handshake: {e}")))?
            }
            TransportConfig::Http { url, .. } => {
                let transport = StreamableHttpClientTransport::from_uri(url.clone());
                ().into_dyn()
                    .serve(transport)
                    .await
                    .map_err(|e| McpConnectError::Handshake(format!("http handshake: {e}")))?
            }
        };

        Ok(Self { config, service })
    }

    /// List tools available on this server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpCallError> {
        let tools = self
            .service
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| McpCallError::ListTools(e.to_string()))?;

        Ok(tools
            .into_iter()
            .map(|t| McpToolInfo {
                name: t.name.to_string(),
                description: t.description.clone().unwrap_or_default().to_string(),
                input_schema: serde_json::to_value(&t.input_schema).unwrap_or_default(),
            })
            .collect())
    }

    /// Call a tool on the server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpCallError> {
        let mut req = rmcp::model::CallToolRequestParams::new(tool_name.to_string());
        req.arguments = params.as_object().cloned();
        let result = self
            .service
            .peer()
            .call_tool(req)
            .await
            .map_err(|e| McpCallError::CallFailed(e.to_string()))?;

        // Convert the tool result content to JSON.
        let content: Vec<serde_json::Value> = result
            .content
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(serde_json::Value::Null))
            .collect();

        Ok(serde_json::Value::Array(content))
    }
}

/// Errors from connecting to an MCP server.
#[derive(Debug, thiserror::Error)]
pub enum McpConnectError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("handshake failed: {0}")]
    Handshake(String),
}

/// Errors from calling MCP tools.
#[derive(Debug, thiserror::Error)]
pub enum McpCallError {
    #[error("list_tools failed: {0}")]
    ListTools(String),
    #[error("tool call failed: {0}")]
    CallFailed(String),
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server", &self.config.name)
            .finish()
    }
}
