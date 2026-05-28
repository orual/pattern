// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! MCP server configuration types.

use serde::{Deserialize, Serialize};

/// Configuration for connecting to an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Human-readable name for this server.
    pub name: String,
    /// Transport configuration.
    pub transport: TransportConfig,
}

/// How to connect to the MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransportConfig {
    /// Stdio transport — spawn a child process.
    Stdio {
        command: String,
        args: Vec<String>,
        #[serde(default)]
        env: std::collections::HashMap<String, String>,
    },
    /// HTTP transport (streamable HTTP / SSE).
    Http {
        url: String,
        #[serde(default)]
        auth: AuthConfig,
    },
}

/// Authentication for HTTP MCP transports.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum AuthConfig {
    #[default]
    None,
    Bearer(String),
    Headers(std::collections::HashMap<String, String>),
}
