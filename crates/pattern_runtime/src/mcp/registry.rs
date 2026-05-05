//! Per-session MCP server registry.
//!
//! Manages live connections to MCP servers. Constructed at session open
//! from persona KDL + project KDL + CC plugin configs.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use pattern_core::mcp::{McpClient, McpServerConfig, McpToolInfo};

/// Per-session registry of connected MCP servers.
#[derive(Default)]
pub struct McpRegistry {
    servers: RwLock<HashMap<String, Arc<McpClient>>>,
}

impl std::fmt::Debug for McpRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRegistry").finish_non_exhaustive()
    }
}

impl McpRegistry {
    /// Load a set of server configs, connecting to each.
    /// Returns results per-server (name, outcome).
    pub async fn load_servers(
        &self,
        configs: &[McpServerConfig],
    ) -> Vec<(String, Result<(), String>)> {
        let mut results = Vec::new();
        for config in configs {
            let name = config.name.clone();
            match McpClient::connect(config.clone()).await {
                Ok(client) => {
                    self.servers.write().await.insert(name.clone(), Arc::new(client));
                    results.push((name, Ok(())));
                }
                Err(e) => {
                    results.push((name, Err(e.to_string())));
                }
            }
        }
        results
    }

    /// List names of currently connected servers.
    pub async fn list_connected(&self) -> Vec<String> {
        self.servers.read().await.keys().cloned().collect()
    }

    /// List tools available on a specific server.
    pub async fn list_tools(&self, server: &str) -> Result<Vec<McpToolInfo>, McpCallError> {
        let servers = self.servers.read().await;
        let client = servers.get(server)
            .ok_or_else(|| McpCallError::ServerNotFound(server.to_string()))?;
        client.list_tools().await
            .map_err(|e| McpCallError::CallFailed(e.to_string()))
    }

    /// Call a tool on a specific server.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpCallError> {
        let servers = self.servers.read().await;
        let client = servers.get(server)
            .ok_or_else(|| McpCallError::ServerNotFound(server.to_string()))?;
        client.call_tool(tool, params).await
            .map_err(|e| McpCallError::CallFailed(e.to_string()))
    }

    /// Disconnect a specific server.
    pub async fn unload(&self, server: &str) -> bool {
        self.servers.write().await.remove(server).is_some()
    }
}

/// Errors from MCP registry operations.
#[derive(Debug, thiserror::Error)]
pub enum McpCallError {
    #[error("MCP server not found: {0}")]
    ServerNotFound(String),
    #[error("MCP call failed: {0}")]
    CallFailed(String),
}
