//! Provider authentication module.
//!
//! This module provides storage for OAuth tokens from AI model providers
//! (Anthropic, OpenAI, etc.), enabling Pattern to maintain authenticated
//! sessions across restarts.

mod oauth;

pub use oauth::ProviderOAuthToken;
