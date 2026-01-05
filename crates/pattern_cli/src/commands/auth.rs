//! OAuth authentication commands for model providers (Anthropic, OpenAI, etc.)
//!
//! These commands manage OAuth tokens for AI model providers. This is separate
//! from ATProto authentication (see atproto.rs for Bluesky auth).

use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use pattern_auth::ProviderOAuthToken;
use pattern_core::config::PatternConfig;
use pattern_core::oauth::{OAuthClient, OAuthProvider, auth_flow::split_callback_code};
use std::io::{self, Write};

use crate::helpers::get_dbs;
use crate::output::{Output, format_relative_time};

/// Login with OAuth for a model provider.
///
/// Starts the OAuth device flow, prompts user to authorize in browser,
/// then exchanges the callback code for tokens.
pub async fn login(provider: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    // Parse provider
    let oauth_provider = match provider.to_lowercase().as_str() {
        "anthropic" => OAuthProvider::Anthropic,
        other => {
            output.error(&format!("Unknown provider: {}", other.bright_red()));
            output.info("Supported providers:", "anthropic");
            return Ok(());
        }
    };

    output.section(&format!("OAuth Login: {}", provider.bright_cyan()));

    // Create OAuth client and start device flow
    let oauth_client = OAuthClient::new(oauth_provider);
    let device_response = oauth_client.start_device_flow().into_diagnostic()?;

    // Display instructions
    output.print("");
    output.info(
        "Get API keys at:",
        "https://console.anthropic.com/settings/keys",
    );
    output.print("");
    output.status("Please visit the URL above and authorize the application.");
    output.status("After authorization, copy the full callback URL or code shown on the page.");
    output.print("");

    // Prompt for the code
    print!("Enter the authorization code: ");
    io::stdout().flush().into_diagnostic()?;

    let mut code_input = String::new();
    io::stdin().read_line(&mut code_input).into_diagnostic()?;
    let (code, state) = split_callback_code(code_input.trim()).into_diagnostic()?;

    // Verify state matches PKCE challenge
    let pkce = device_response
        .pkce_challenge
        .ok_or_else(|| miette::miette!("No PKCE challenge found - this shouldn't happen"))?;

    if state != pkce.state {
        output.error("State mismatch - authorization may have been tampered with");
        return Ok(());
    }

    // Exchange code for token
    let token_response = oauth_client
        .exchange_code(code, &pkce)
        .await
        .into_diagnostic()?;

    output.success("Authentication successful!");

    // Log token details
    tracing::info!(
        "Received OAuth token - has refresh_token: {}, expires_in: {} seconds",
        token_response.refresh_token.is_some(),
        token_response.expires_in
    );

    if token_response.refresh_token.is_none() {
        output.warning("Note: No refresh token received. You'll need to re-authenticate when the token expires.");
    }

    // Calculate expiry and create token for storage
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::seconds(token_response.expires_in as i64);

    let token = ProviderOAuthToken {
        provider: oauth_provider.as_str().to_string(),
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        expires_at: Some(expires_at),
        scope: token_response.scope,
        session_id: None,
        created_at: now,
        updated_at: now,
    };

    // Store token in database
    let dbs = get_dbs(config).await?;
    dbs.auth
        .set_provider_oauth_token(&token)
        .await
        .into_diagnostic()?;

    output.success(&format!("Token stored for provider: {}", oauth_provider));
    output.info(
        "You can now use OAuth authentication with this provider.",
        "",
    );

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
