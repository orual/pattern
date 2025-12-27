//! OAuth authentication commands for model providers (Anthropic, OpenAI, etc.)
//!
//! These commands manage OAuth tokens for AI model providers. This is separate
//! from ATProto authentication (see atproto.rs for Bluesky auth).
//!
//! Note: Provider OAuth (e.g., Anthropic) requires browser-based authorization
//! which is not fully supported in CLI. Use the web UI for initial authentication,
//! or use API keys directly via environment variables.

use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::PatternConfig;

use crate::helpers::get_dbs;
use crate::output::{Output, format_relative_time};

/// Login with OAuth for a model provider.
///
/// Provider OAuth (Anthropic, OpenAI, etc.) requires browser-based authorization
/// that cannot be fully handled in CLI. This command shows guidance on how to
/// authenticate.
pub async fn login(provider: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    // Validate provider
    let provider_lower = provider.to_lowercase();
    match provider_lower.as_str() {
        "anthropic" => {}
        other => {
            output.error(&format!("Unknown provider: {}", other.bright_red()));
            output.info("Supported providers:", "anthropic");
            return Ok(());
        }
    }

    output.section(&format!("OAuth Login: {}", provider.bright_cyan()));

    // Provider OAuth requires browser-based flow
    output.print("");
    output.warning("Provider OAuth requires browser-based authorization.");
    output.print("");
    output.status("For Anthropic API access, you have two options:");
    output.print("");
    output.list_item("Use an API key via the ANTHROPIC_API_KEY environment variable");
    output.list_item("Use the Pattern web UI for OAuth authentication (when available)");
    output.print("");
    output.info(
        "Get API keys at:",
        "https://console.anthropic.com/settings/keys",
    );
    output.print("");

    // Check if we already have a token stored
    let dbs = get_dbs(config).await?;
    match dbs.auth.get_provider_oauth_token(&provider_lower).await {
        Ok(Some(token)) => {
            output.success("You already have an OAuth token stored for this provider.");
            if token.is_expired() {
                output.warning("However, the token has expired.");
            } else if token.needs_refresh() {
                output.warning("The token will expire soon and needs refresh.");
            }
        }
        Ok(None) => {
            output.status("No OAuth token currently stored for this provider.");
        }
        Err(e) => {
            output.warning(&format!("Could not check existing tokens: {}", e));
        }
    }

    Ok(())
}

/// Show authentication status for all providers.
///
/// Lists all stored OAuth tokens and their status (valid, expiring, expired).
pub async fn status(config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    output.section("Provider OAuth Status");

    let dbs = get_dbs(config).await?;

    let tokens = match dbs.auth.list_provider_oauth_tokens().await {
        Ok(t) => t,
        Err(e) => {
            output.error(&format!("Failed to list tokens: {}", e));
            return Ok(());
        }
    };

    if tokens.is_empty() {
        output.status("No OAuth tokens stored.");
        output.print("");
        output.info(
            "Note:",
            "Most providers use API keys via environment variables.",
        );
        output.info("Example:", "export ANTHROPIC_API_KEY=your-key-here");
        return Ok(());
    }

    for token in tokens {
        output.print("");
        output.info(
            "Provider:",
            &token.provider.bright_cyan().bold().to_string(),
        );

        // Determine status
        let status = if token.is_expired() {
            "EXPIRED".bright_red().bold().to_string()
        } else if token.needs_refresh() {
            "NEEDS REFRESH".yellow().to_string()
        } else {
            "VALID".bright_green().to_string()
        };

        output.info("Status:", &status);

        if let Some(expires_at) = token.expires_at {
            if token.is_expired() {
                output.info("Expired:", &format_relative_time(expires_at));
            } else {
                output.info("Expires:", &format_relative_time(expires_at));
            }
        } else {
            output.info("Expires:", "Never");
        }

        if let Some(scope) = &token.scope {
            output.info("Scope:", scope);
        }

        output.info("Last updated:", &format_relative_time(token.updated_at));
    }

    Ok(())
}

/// Logout from a provider (remove stored tokens).
///
/// Deletes the OAuth token for the specified provider.
pub async fn logout(provider: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    let provider_lower = provider.to_lowercase();

    output.section(&format!("OAuth Logout: {}", provider.bright_cyan()));

    let dbs = get_dbs(config).await?;

    // Check if token exists
    match dbs.auth.get_provider_oauth_token(&provider_lower).await {
        Ok(Some(_)) => {
            // Token exists, delete it
            if let Err(e) = dbs.auth.delete_provider_oauth_token(&provider_lower).await {
                output.error(&format!("Failed to delete token: {}", e));
                return Ok(());
            }
            output.success(&format!(
                "Successfully logged out from {}.",
                provider.bright_green()
            ));
        }
        Ok(None) => {
            output.warning(&format!("No OAuth token found for provider: {}", provider));
        }
        Err(e) => {
            output.error(&format!("Failed to check token: {}", e));
        }
    }

    Ok(())
}
