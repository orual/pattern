//! Parse CC plugin MCP server declarations into McpServerConfig.
//!
//! Sources (priority order):
//! 1. Inline `mcpServers` in plugin.json manifest
//! 2. Standalone `.mcp.json` at plugin root

use std::collections::HashMap;
use std::path::Path;

use pattern_core::mcp::{AuthConfig, McpServerConfig, TransportConfig};

/// Parse MCP server configs from a CC plugin's manifest and/or .mcp.json file.
pub fn parse_mcp_servers(
    plugin_root: &Path,
    manifest_mcp_servers: &serde_json::Value,
) -> Vec<McpServerConfig> {
    // Try inline mcpServers from manifest first
    if let Some(servers) = try_parse_mcp_json(manifest_mcp_servers, plugin_root) {
        if !servers.is_empty() {
            return servers;
        }
    }

    // Fall back to .mcp.json file at plugin root
    let mcp_json_path = plugin_root.join(".mcp.json");
    if mcp_json_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&mcp_json_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(servers_obj) = val.get("mcpServers") {
                    if let Some(servers) = try_parse_mcp_json(servers_obj, plugin_root) {
                        return servers;
                    }
                }
            }
        }
    }

    Vec::new()
}

/// Parse a JSON object of shape { "name": { "command": ..., "args": [...], "env": {...} } }
fn try_parse_mcp_json(
    servers_value: &serde_json::Value,
    plugin_root: &Path,
) -> Option<Vec<McpServerConfig>> {
    let obj = servers_value.as_object()?;
    let mut configs = Vec::new();

    for (name, server_def) in obj {
        let server_obj = server_def.as_object()?;

        // Collect env vars
        let env: HashMap<String, String> = server_obj
            .get("env")
            .and_then(|e| e.as_object())
            .map(|e| {
                e.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        // Determine transport
        let transport = if let Some(command) = server_obj.get("command").and_then(|c| c.as_str()) {
            // Resolve ${CLAUDE_PLUGIN_ROOT} in command
            let resolved_cmd = command.replace("${CLAUDE_PLUGIN_ROOT}", &plugin_root.to_string_lossy());
            let args: Vec<String> = server_obj
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.replace("${CLAUDE_PLUGIN_ROOT}", &plugin_root.to_string_lossy()))
                        .collect()
                })
                .unwrap_or_default();

            TransportConfig::Stdio {
                command: resolved_cmd,
                args,
                env,
            }
        } else if let Some(url) = server_obj.get("url").and_then(|u| u.as_str()) {
            TransportConfig::Http {
                url: url.to_string(),
                auth: AuthConfig::None, // TODO: parse auth from headers
            }
        } else {
            continue; // Skip entries we can't parse
        };

        configs.push(McpServerConfig {
            name: name.clone(),
            transport,
        });
    }

    Some(configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_stdio_server() {
        let servers = json!({
            "my-server": {
                "command": "npx",
                "args": ["@modelcontextprotocol/server-github"],
                "env": { "GITHUB_TOKEN": "abc123" }
            }
        });
        let result = parse_mcp_servers(Path::new("/tmp/plugin"), &servers);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "my-server");
        match &result[0].transport {
            TransportConfig::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &["@modelcontextprotocol/server-github"]);
                assert_eq!(env.get("GITHUB_TOKEN").unwrap(), "abc123");
            }
            _ => panic!("expected stdio transport"),
        }
    }

    #[test]
    fn parse_http_server() {
        let servers = json!({
            "api": {
                "url": "https://api.example.com/mcp"
            }
        });
        let result = parse_mcp_servers(Path::new("/tmp/plugin"), &servers);
        assert_eq!(result.len(), 1);
        match &result[0].transport {
            TransportConfig::Http { url, .. } => {
                assert_eq!(url, "https://api.example.com/mcp");
            }
            _ => panic!("expected http transport"),
        }
    }

    #[test]
    fn resolves_plugin_root_variable() {
        let servers = json!({
            "local": {
                "command": "${CLAUDE_PLUGIN_ROOT}/bin/server",
                "args": ["--config", "${CLAUDE_PLUGIN_ROOT}/config.json"]
            }
        });
        let result = parse_mcp_servers(Path::new("/home/user/plugins/my-plugin"), &servers);
        match &result[0].transport {
            TransportConfig::Stdio { command, args, .. } => {
                assert_eq!(command, "/home/user/plugins/my-plugin/bin/server");
                assert_eq!(args[1], "/home/user/plugins/my-plugin/config.json");
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn empty_on_missing() {
        let servers = json!(null);
        let result = parse_mcp_servers(Path::new("/tmp/nonexistent"), &servers);
        assert!(result.is_empty());
    }
}
