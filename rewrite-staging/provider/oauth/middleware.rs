// MOVING TO: pattern_provider/src/auth/middleware.rs
// ORIGIN: crates/pattern_core/src/oauth/middleware.rs
// PHASE: 4
// RESHAPE: Absorbs into three-tier resolver; pattern_auth crate retires in same phase
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Request transformation middleware for OAuth-authenticated requests
//!
//! This middleware transforms requests to match Anthropic's OAuth API requirements,
//! particularly converting system prompts from string to array format.

use crate::oauth::OAuthToken;
use async_trait::async_trait;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next, Result as MiddlewareResult};
use serde_json::{Value, json};
use std::sync::Arc;

/// Middleware that transforms requests for Anthropic OAuth API
pub struct AnthropicOAuthMiddleware {
    /// Optional OAuth token to use for authentication
    token: Option<Arc<OAuthToken>>,
}

impl AnthropicOAuthMiddleware {
    /// Create a new middleware instance
    pub fn new(token: Option<Arc<OAuthToken>>) -> Self {
        Self { token }
    }

    /// Transform the system prompt from string to array format
    fn transform_system_prompt(body: &mut Value) {
        // Prepend Claude Code identification to system prompt array
        let claude_code_obj = json!({
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude."
        });

        // Handle both string and array system prompts
        match body.get_mut("system") {
            Some(Value::Array(system_array)) => {
                // Prepend to existing array
                system_array.insert(0, claude_code_obj);
            }
            Some(Value::String(existing_str)) => {
                // Convert string to array format
                let existing_obj = json!({
                    "type": "text",
                    "text": existing_str
                });
                body["system"] = json!([claude_code_obj, existing_obj]);
            }
            _ => {
                // No system prompt, create new array
                body["system"] = json!([claude_code_obj]);
            }
        }
    }

    /// Add cache control to optimize token usage
    fn add_cache_control(body: &mut Value) {
        // This follows the pattern from the proxy:
        // Add cache_control to first 2 system messages
        if let Some(messages) = body.get_mut("messages") {
            if let Some(messages_array) = messages.as_array_mut() {
                let mut system_count = 0;
                for msg in messages_array.iter_mut() {
                    if msg.get("role") == Some(&Value::String("system".to_string())) {
                        if let Some(msg_obj) = msg.as_object_mut() {
                            msg_obj
                                .insert("cache_control".to_string(), json!({"type": "ephemeral"}));
                        }
                        system_count += 1;
                        if system_count >= 2 {
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Middleware for AnthropicOAuthMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> MiddlewareResult<Response> {
        // Only transform requests to Anthropic's API
        if let Some(host) = req.url().host_str() {
            if host.contains("anthropic.com") {
                // Add OAuth bearer token if available
                if let Some(token) = &self.token {
                    let headers = req.headers_mut();
                    headers.insert(
                        "Authorization",
                        format!("Bearer {}", token.access_token).parse().unwrap(),
                    );

                    // Add the OAuth beta header
                    headers.insert(
                        "anthropic-beta",
                        "oauth-2025-04-20,computer-use-2025-01-24,fine-grained-tool-streaming-2025-05-14"
                            .parse()
                            .unwrap(),
                    );
                }

                // Transform the request body for messages endpoint
                if req.url().path().contains("/messages") {
                    // Get the body bytes - need to consume and replace
                    if let Some(body) = req.body() {
                        let body_bytes = body.as_bytes().unwrap_or(&[]);

                        if let Ok(mut json_body) = serde_json::from_slice::<Value>(body_bytes) {
                            // Transform system prompt
                            Self::transform_system_prompt(&mut json_body);

                            // Add cache control
                            Self::add_cache_control(&mut json_body);

                            // Set the modified body back
                            if let Ok(new_body) = serde_json::to_vec(&json_body) {
                                let new_len = new_body.len();
                                *req.body_mut() = Some(new_body.into());

                                // Update content-length header
                                let headers = req.headers_mut();
                                headers
                                    .insert("Content-Length", new_len.to_string().parse().unwrap());
                            }
                        }
                    }
                }
            }
        }

        // Continue with the request
        next.run(req, extensions).await
    }
}

/// Builder for creating a reqwest client with OAuth middleware
pub struct OAuthClientBuilder {
    token: Option<Arc<OAuthToken>>,
    base_client: Option<reqwest::Client>,
}

impl OAuthClientBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            token: None,
            base_client: None,
        }
    }

    /// Set the OAuth token to use
    pub fn with_token(mut self, token: Arc<OAuthToken>) -> Self {
        self.token = Some(token);
        self
    }

    /// Use a specific base client
    pub fn with_base_client(mut self, client: reqwest::Client) -> Self {
        self.base_client = Some(client);
        self
    }

    /// Build the client with middleware
    pub fn build(self) -> reqwest_middleware::ClientWithMiddleware {
        let base_client = self.base_client.unwrap_or_else(reqwest::Client::new);
        let middleware = AnthropicOAuthMiddleware::new(self.token);

        reqwest_middleware::ClientBuilder::new(base_client)
            .with(middleware)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_prompt_transformation() {
        let mut body = json!({
            "system": "You are a helpful assistant",
            "messages": []
        });

        AnthropicOAuthMiddleware::transform_system_prompt(&mut body);

        // Should be converted to array format
        assert!(body["system"].is_array());
        let system_array = body["system"].as_array().unwrap();
        assert_eq!(system_array.len(), 2);

        // First element should be Claude Code identification
        assert_eq!(
            system_array[0]["text"].as_str().unwrap(),
            "You are Claude Code, Anthropic's official CLI for Claude."
        );

        // Second element should be original system prompt
        assert_eq!(
            system_array[1]["text"].as_str().unwrap(),
            "You are a helpful assistant"
        );
    }

    #[test]
    fn test_cache_control_addition() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "System message 1"},
                {"role": "user", "content": "User message"},
                {"role": "system", "content": "System message 2"},
                {"role": "assistant", "content": "Assistant message"},
            ]
        });

        AnthropicOAuthMiddleware::add_cache_control(&mut body);

        let messages = body["messages"].as_array().unwrap();

        // First system message should have cache_control
        assert!(messages[0]["cache_control"].is_object());

        // User message should not have cache_control
        assert!(messages[1]["cache_control"].is_null());

        // Second system message should have cache_control
        assert!(messages[2]["cache_control"].is_object());

        // Assistant message should not have cache_control
        assert!(messages[3]["cache_control"].is_null());
    }
}
