//! `pattern auth {login,status,clear}` subcommand implementations.
//!
//! Mirrors the auth surface in `pattern-test-cli` so the production
//! binary has the same credential-management toolkit. The actual
//! credential resolution and PKCE flow live in `pattern_provider`;
//! this module is thin wiring over those primitives.
//!
//! Provider support today:
//!
//!   - `anthropic`: full chain (stored OAuth keyring/JSON → API key
//!                  env → session-pickup from claude-code), plus
//!                  PKCE flow on `login` when no creds are present.
//!   - `gemini`:    chain construction only (API key resolution).
//!                  No PKCE flow yet — Google OAuth flow lands when
//!                  Gemini provider work picks up.
//!
//! `--provider` accepts both today; subcommands gate on whether the
//! requested provider supports the requested operation.
//!
//! On Unix the JSON credential fallback is created with `0700` perms;
//! on Windows we fall back to the user's `%APPDATA%` ACL.

use std::io::Write;
use std::sync::Arc;

use clap::{Args, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result as MietteResult, miette};

#[cfg(feature = "oauth")]
use pattern_core::types::provider::ProviderCredential;
use pattern_provider::auth::{AnthropicAuthChain, CredentialChain, GeminiAuthChain, ResolvedCredential};
#[cfg(feature = "oauth")]
use pattern_provider::auth::{PkceTier, SessionPickupTier};
#[cfg(feature = "oauth")]
use pattern_provider::creds_store::{
    CredsStore, CredsStoreResolver, JsonFallbackStore, KeyringStore,
};
use secrecy::ExposeSecret;

// ---------------------------------------------------------------------------
// CLI definitions
// ---------------------------------------------------------------------------

/// `pattern auth ...` group.
#[derive(Args)]
pub struct AuthCmd {
    #[command(subcommand)]
    pub sub: AuthSub,
}

/// Auth subcommands.
#[derive(Subcommand)]
pub enum AuthSub {
    /// Run the interactive PKCE flow and persist the resulting token
    /// to the local creds store. Always runs PKCE — does not check
    /// whether other tiers (api-key, session-pickup) would resolve
    /// first. Use `auth status` to see which tier the resolver
    /// currently picks.
    ///
    /// Anthropic only — other providers don't have PKCE flows wired yet.
    Login {
        /// Provider to authenticate against. Defaults to `anthropic`.
        #[arg(long, value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,
    },

    /// Resolve the credential chain and print the active tier + token
    /// shape, without prompting. Useful for verifying which auth path
    /// the daemon will resolve at session open.
    Status {
        /// Provider to query. Defaults to `anthropic`.
        #[arg(long, value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,
    },

    /// Delete the stored OAuth credential for a provider (keyring +
    /// JSON fallback). The next `login` falls through to session-pickup
    /// (if available) or starts a fresh PKCE flow. Does NOT touch
    /// claude-code's `~/.claude/.credentials.json`.
    Clear {
        /// Provider whose stored credential to delete. Defaults to `anthropic`.
        #[arg(long, value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,
    },
}

/// Providers the auth CLI knows how to construct chains for.
///
/// Mirrors the test-cli enum so behaviour is consistent across the
/// two binaries. Add new providers here as their auth chains land.
#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ProviderKind {
    Anthropic,
    Gemini,
}

impl ProviderKind {
    fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Run the `pattern auth ...` subcommand.
pub async fn cmd_auth(cmd: AuthCmd) -> MietteResult<()> {
    match cmd.sub {
        AuthSub::Login { provider } => cmd_login(provider).await,
        AuthSub::Status { provider } => cmd_status(provider).await,
        AuthSub::Clear { provider } => cmd_clear(provider).await,
    }
}

// ---------------------------------------------------------------------------
// login
// ---------------------------------------------------------------------------

async fn cmd_login(provider: ProviderKind) -> MietteResult<()> {
    match provider {
        ProviderKind::Anthropic => {
            #[cfg(feature = "oauth")]
            {
                eprintln!("starting PKCE flow for provider=anthropic");
                let token = run_pkce_interactive().await?;
                eprintln!("✓ PKCE flow completed");
                eprintln!("  tier: pkce (freshly obtained)");
                eprintln!(
                    "  access_token_len: {}",
                    token.access_token.expose_secret().len()
                );
                eprintln!(
                    "  refresh_token: {}",
                    if token.refresh_token.is_some() {
                        "present"
                    } else {
                        "absent"
                    }
                );
                eprintln!("  expires_at: {:?}", token.expires_at);
                eprintln!("  scope: {:?}", token.scope);
                Ok(())
            }
            #[cfg(not(feature = "oauth"))]
            {
                Err(miette!(
                    "anthropic login requires the `oauth` feature; rebuild without `--no-default-features`"
                ))
            }
        }
        ProviderKind::Gemini => Err(miette!(
            "gemini does not have a PKCE flow wired yet. \
             use ANTHROPIC_API_KEY-style env auth, or wait until the gemini auth chain lands"
        )),
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

async fn cmd_status(provider: ProviderKind) -> MietteResult<()> {
    let chain = build_chain(provider).await?;

    eprintln!(
        "resolving credential chain for provider={}",
        provider.as_str()
    );

    match chain.resolve().await {
        Ok(resolved) => {
            print_resolved(&resolved);
            Ok(())
        }
        Err(e) => Err(miette!(
            "no credential resolved for provider={}: {e}\n\
             run `pattern auth login --provider {}` to authenticate",
            provider.as_str(),
            provider.as_str()
        )),
    }
}

// ---------------------------------------------------------------------------
// clear
// ---------------------------------------------------------------------------

async fn cmd_clear(provider: ProviderKind) -> MietteResult<()> {
    #[cfg(feature = "oauth")]
    {
        let primary: Arc<dyn CredsStore> = Arc::new(KeyringStore::new());
        let fallback: Arc<dyn CredsStore> =
            Arc::new(JsonFallbackStore::new().into_diagnostic()?);
        let store = CredsStoreResolver::new(primary, fallback);

        eprintln!(
            "clearing stored credentials for provider={} (keyring + JSON fallback)",
            provider.as_str()
        );
        eprintln!("  NOTE: claude-code's ~/.claude/.credentials.json is NOT touched.");

        store
            .delete(provider.as_str())
            .await
            .into_diagnostic()
            .map_err(|e| miette!("clear failed: {e}"))?;

        eprintln!("✓ cleared. next `auth login` falls through to session-pickup or PKCE.");
        Ok(())
    }
    #[cfg(not(feature = "oauth"))]
    {
        let _ = provider;
        Err(miette!(
            "clear requires the `oauth` feature (keyring + JSON fallback are \
             only compiled in under that feature)"
        ))
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn print_resolved(r: &ResolvedCredential) {
    eprintln!("✓ credential resolved");
    eprintln!("  tier: {:?}", r.source);
    eprintln!("  provider: {}", r.token.provider);
    eprintln!(
        "  access_token_len: {} chars",
        r.token.access_token.expose_secret().len()
    );
    eprintln!(
        "  refresh_token: {}",
        if r.token.refresh_token.is_some() {
            "present"
        } else {
            "absent"
        }
    );
    eprintln!("  expires_at: {:?}", r.token.expires_at);
    eprintln!("  scope: {:?}", r.token.scope);
    eprintln!("  session_id: {:?}", r.token.session_id);
}

async fn build_chain(
    provider: ProviderKind,
) -> MietteResult<Arc<dyn CredentialChain>> {
    match provider {
        ProviderKind::Anthropic => {
            #[cfg(feature = "oauth")]
            {
                let session_pickup = SessionPickupTier::default();
                let pkce = Arc::new(PkceTier::anthropic());
                let primary: Arc<dyn CredsStore> = Arc::new(KeyringStore::new());
                let fallback: Arc<dyn CredsStore> =
                    Arc::new(JsonFallbackStore::new().into_diagnostic()?);
                let creds_store: Arc<dyn CredsStore> =
                    Arc::new(CredsStoreResolver::new(primary, fallback));

                let chain: Arc<dyn CredentialChain> = Arc::new(AnthropicAuthChain::with_oauth(
                    session_pickup,
                    pkce,
                    creds_store,
                ));
                Ok(chain)
            }
            #[cfg(not(feature = "oauth"))]
            {
                let chain: Arc<dyn CredentialChain> =
                    Arc::new(AnthropicAuthChain::api_key_only());
                Ok(chain)
            }
        }
        ProviderKind::Gemini => {
            let chain: Arc<dyn CredentialChain> = Arc::new(GeminiAuthChain::new());
            Ok(chain)
        }
    }
}

// ---------------------------------------------------------------------------
// Interactive PKCE flow
// ---------------------------------------------------------------------------

#[cfg(feature = "oauth")]
async fn run_pkce_interactive() -> MietteResult<ProviderCredential> {
    let tier = PkceTier::anthropic();
    let pending = tier.begin_auth();

    eprintln!();
    eprintln!("────────────────────────────────────────────────────────────");
    eprintln!("Open this URL in your browser and complete the auth flow:");
    eprintln!();
    eprintln!("  {}", pending.authorize_url());
    eprintln!();
    eprintln!("After approving, the browser redirects to a URL containing");
    eprintln!("`?code=<code>&state=<state>`. Paste the ENTIRE redirect URL,");
    eprintln!("or just `<code>#<state>`, below and press Enter.");
    eprintln!("────────────────────────────────────────────────────────────");
    eprint!("paste> ");
    std::io::stderr().flush().into_diagnostic()?;

    let mut line = String::new();
    std::io::stdin().read_line(&mut line).into_diagnostic()?;
    let pasted = line.trim();

    let token = tier
        .complete_manual(pending, pasted)
        .await
        .into_diagnostic()?;

    // Persist so subsequent `auth` resolves find the stored token via
    // the creds_store tier rather than re-running PKCE.
    let primary: Arc<dyn CredsStore> = Arc::new(KeyringStore::new());
    let fallback: Arc<dyn CredsStore> = Arc::new(JsonFallbackStore::new().into_diagnostic()?);
    let store = CredsStoreResolver::new(primary, fallback);
    if let Err(e) = store.put(&token).await {
        eprintln!("⚠ token obtained but store write failed: {e}");
        eprintln!("  (run `auth login` again to retry storage; session-pickup path remains usable)");
    } else {
        eprintln!("✓ token stored via creds_store");
    }

    Ok(token)
}
