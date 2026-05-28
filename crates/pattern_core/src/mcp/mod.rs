// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
