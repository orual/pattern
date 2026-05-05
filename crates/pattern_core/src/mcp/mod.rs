//! MCP (Model Context Protocol) client.
//!
//! Feature-gated behind `mcp-client`. Provides:
//! - `McpServerConfig` — how to connect to an MCP server
//! - `McpClient` — manages a connection to a single MCP server
//! - `McpToolInfo` — metadata about a discovered tool

mod config;
mod client;

pub use config::{McpServerConfig, TransportConfig, AuthConfig};
pub use client::{McpClient, McpToolInfo};
