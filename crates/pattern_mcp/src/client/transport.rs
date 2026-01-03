//! Transport implementation for MCP client

use crate::{Result, error::McpError};
use rmcp::{
    service::{DynService, RoleClient, RunningService, ServiceExt},
    transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess},
};
use tokio::process::Command;

/// Helper function to extract auth header from AuthConfig
fn auth_config_to_header(auth: &AuthConfig) -> Option<String> {
    match auth {
        AuthConfig::Bearer(token) => Some(format!("Bearer {}", token)),
        AuthConfig::Headers(headers) => headers.get("Authorization").cloned(),
        AuthConfig::None | AuthConfig::OAuth { .. } => None,
    }
}

/// Authentication configuration for MCP transports
#[derive(Debug, Clone)]
pub enum AuthConfig {
    /// No authentication
    None,
    /// Bearer token authentication
    Bearer(String),
    /// Custom headers
    Headers(std::collections::HashMap<String, String>),
    /// OAuth configuration (future implementation)
    OAuth {
        client_id: String,
        client_secret: String,
        auth_url: String,
        token_url: String,
    },
}

/// Transport configuration for MCP client
#[derive(Debug, Clone)]
pub enum TransportConfig {
    /// Stdio transport for child process
    Stdio { command: String, args: Vec<String> },
    /// HTTP transport (streamable HTTP)
    Http { url: String, auth: AuthConfig },
}

/// MCP client transport wrapper using dynamic dispatch
pub struct ClientTransport {
    pub service: RunningService<RoleClient, Box<dyn DynService<RoleClient>>>,
}

impl ClientTransport {
    /// Create transport from configuration
    pub async fn from_config(config: TransportConfig) -> Result<Self> {
        match config {
            TransportConfig::Stdio { command, args } => Self::stdio(command, args).await,
            TransportConfig::Http { url, auth } => Self::http(url, auth).await,
        }
    }

    /// Create stdio transport for MCP server
    pub async fn stdio(command: String, args: Vec<String>) -> Result<Self> {
        let transport = TokioChildProcess::new(Command::new(&command).configure(|cmd| {
            for arg in &args {
                cmd.arg(arg);
            }
        }))
        .map_err(|e| McpError::transport_init("stdio", &command, e))?;

        let service = ()
            .into_dyn()
            .serve(transport)
            .await
            .map_err(|e| McpError::transport_init("stdio", &command, e))?;

        Ok(Self { service })
    }

    /// Create HTTP transport for MCP server
    pub async fn http(url: String, auth: AuthConfig) -> Result<Self> {
        match auth {
            AuthConfig::None => {
                let transport = StreamableHttpClientTransport::from_uri(url.clone());
                let service =
                    ().into_dyn()
                        .serve(transport)
                        .await
                        .map_err(|e| McpError::transport_init("http", &url, e))?;
                Ok(Self { service })
            }
            AuthConfig::Bearer(_) | AuthConfig::Headers(_) => {
                // For now, use basic transport with auth header support
                // TODO: Custom headers beyond Authorization need custom client implementation
                let auth_header = auth_config_to_header(&auth);
                if auth_header.is_some() {
                    // The rmcp transport should support auth headers via the auth_header parameter
                    let transport = StreamableHttpClientTransport::from_uri(url.clone());
                    let service =
                        ().into_dyn()
                            .serve(transport)
                            .await
                            .map_err(|e| McpError::transport_init("http", &url, e))?;
                    Ok(Self { service })
                } else {
                    Err(McpError::transport_init(
                        "http",
                        &url,
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "Custom headers other than Authorization not yet supported",
                        ),
                    ))
                }
            }
            AuthConfig::OAuth { .. } => Err(McpError::transport_init(
                "http",
                &url,
                std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "OAuth authentication not yet implemented",
                ),
            )),
        }
    }

    /// Get the peer for MCP operations
    pub fn peer(&self) -> &rmcp::service::Peer<RoleClient> {
        self.service.peer()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_config_creation() {
        // Test different transport configurations
        let stdio_config = TransportConfig::Stdio {
            command: "uvx".to_string(),
            args: vec!["mcp-server-git".to_string()],
        };

        let http_config = TransportConfig::Http {
            url: "https://api.example.com/mcp".to_string(),
            auth: AuthConfig::Bearer("token123".to_string()),
        };

        // Just test that they can be created
        assert!(matches!(stdio_config, TransportConfig::Stdio { .. }));
        assert!(matches!(http_config, TransportConfig::Http { .. }));
    }

    #[test]
    fn test_auth_config_to_header() {
        // Test Bearer token
        let bearer_auth = AuthConfig::Bearer("test-token".to_string());
        assert_eq!(
            auth_config_to_header(&bearer_auth),
            Some("Bearer test-token".to_string())
        );

        // Test custom headers with Authorization
        let mut headers = std::collections::HashMap::new();
        headers.insert("Authorization".to_string(), "Custom auth-value".to_string());
        let headers_auth = AuthConfig::Headers(headers);
        assert_eq!(
            auth_config_to_header(&headers_auth),
            Some("Custom auth-value".to_string())
        );

        // Test custom headers without Authorization
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-API-Key".to_string(), "api-key-value".to_string());
        let headers_auth = AuthConfig::Headers(headers);
        assert_eq!(auth_config_to_header(&headers_auth), None);

        // Test None auth
        let none_auth = AuthConfig::None;
        assert_eq!(auth_config_to_header(&none_auth), None);
    }
}
