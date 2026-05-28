// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
//!   - `openai`:    codex-OAuth (PKCE loopback on port 1455/1457 with
//!                  device-code fallback) + interop with codex CLI's
//!                  `~/.codex/.auth.json` storage. `--headless` forces
//!                  device-code; `--codex-home` overrides $CODEX_HOME.
//!   - `gemini`:    chain construction only (API key resolution).
//!                  No PKCE flow yet — Google OAuth flow lands when
//!                  Gemini provider work picks up.
//!
//! `--provider` accepts all three today; subcommands gate on whether the
//! requested provider supports the requested operation.
//!
//! On Unix the JSON credential fallback is created with `0700` perms;
//! on Windows we fall back to the user's `%APPDATA%` ACL.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result as MietteResult, miette};

#[cfg(feature = "oauth")]
use pattern_core::types::provider::ProviderCredential;
use pattern_provider::auth::{AnthropicAuthChain, CredentialChain, GeminiAuthChain, ResolvedCredential};
#[cfg(feature = "oauth")]
use pattern_provider::auth::{PkceTier, SessionPickupTier};
#[cfg(feature = "oauth")]
use pattern_provider::auth::{
    CodexAuthStore, CodexLoginHandle, CodexOAuthConfig, CodexTokenSet, LoginFlow,
    OpenAiAuthChain, begin_login as codex_begin_login, complete_login as codex_complete_login,
};
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
    /// Run the interactive auth flow for a provider and persist the
    /// resulting token to the local creds store. Always runs the
    /// auth flow — does not check whether other tiers (api-key,
    /// session-pickup) would resolve first. Use `auth status` to see
    /// which tier the resolver currently picks.
    Login {
        /// Provider to authenticate against. Defaults to `anthropic`.
        #[arg(value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,

        /// Force device-code flow instead of PKCE loopback. Useful for
        /// SSH sessions, headless environments, or any setup where
        /// opening a local TCP listener for the OAuth redirect is
        /// undesirable. OpenAI only — Anthropic doesn't have a
        /// device-code flow wired.
        #[arg(long, default_value_t = false)]
        headless: bool,

        /// Override `$CODEX_HOME` for OpenAI codex storage. Defaults to
        /// the `CODEX_HOME` env var if set, otherwise `~/.codex`.
        /// OpenAI only.
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },

    /// Resolve the credential chain and print the active tier + token
    /// shape, without prompting. Useful for verifying which auth path
    /// the daemon will resolve at session open.
    Status {
        /// Provider to query. Defaults to `anthropic`.
        #[arg(value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,

        /// Override `$CODEX_HOME` for OpenAI codex storage. OpenAI only.
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },

    /// Delete the stored OAuth credential for a provider (keyring +
    /// JSON fallback / codex `.auth.json` depending on provider). The
    /// next `login` re-runs the auth flow. Does NOT touch any other
    /// tool's credentials (claude-code, codex CLI, etc.).
    Clear {
        /// Provider whose stored credential to delete. Defaults to `anthropic`.
        #[arg(value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,

        /// Override `$CODEX_HOME` for OpenAI codex storage. OpenAI only.
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
}

/// Providers the auth CLI knows how to construct chains for.
///
/// Mirrors the test-cli enum so behaviour is consistent across the
/// two binaries. Add new providers here as their auth chains land.
#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ProviderKind {
    Anthropic,
    Openai,
    Gemini,
}

impl ProviderKind {
    fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Openai => "openai",
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
        AuthSub::Login {
            provider,
            headless,
            codex_home,
        } => cmd_login(provider, headless, codex_home).await,
        AuthSub::Status {
            provider,
            codex_home,
        } => cmd_status(provider, codex_home).await,
        AuthSub::Clear {
            provider,
            codex_home,
        } => cmd_clear(provider, codex_home).await,
    }
}

// ---------------------------------------------------------------------------
// login
// ---------------------------------------------------------------------------

async fn cmd_login(
    provider: ProviderKind,
    headless: bool,
    codex_home: Option<PathBuf>,
) -> MietteResult<()> {
    match provider {
        ProviderKind::Anthropic => {
            if headless {
                eprintln!(
                    "note: --headless is ignored for anthropic (no device-code flow); \
                     using manual-paste PKCE"
                );
            }
            if codex_home.is_some() {
                eprintln!("note: --codex-home is ignored for anthropic");
            }
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
        ProviderKind::Openai => {
            #[cfg(feature = "oauth")]
            {
                let flow = if headless {
                    LoginFlow::DeviceCode
                } else {
                    LoginFlow::Auto
                };
                let store = codex_store(codex_home.clone())?;
                eprintln!(
                    "starting codex OAuth flow for provider=openai (flow={flow:?}, codex_home={})",
                    store.codex_home().display()
                );
                let token_set = run_codex_login(flow).await?;
                persist_codex_token_set(&store, &token_set).await?;
                eprintln!("✓ codex OAuth flow completed");
                eprintln!("  tier: stored_oauth (just minted)");
                eprintln!(
                    "  access_token_len: {}",
                    token_set.access_token.expose_secret().len()
                );
                eprintln!("  refresh_token: present");
                eprintln!("  expires_at: {}", token_set.expires_at);
                eprintln!(
                    "  account_id: {}",
                    token_set.account_id.as_deref().unwrap_or("(none)")
                );
                if let Some(plan) = &token_set.claims.chatgpt_plan_type {
                    eprintln!("  plan: {plan}");
                }
                if let Some(email) = &token_set.claims.email {
                    eprintln!("  email: {email}");
                }
                Ok(())
            }
            #[cfg(not(feature = "oauth"))]
            {
                let _ = (headless, codex_home);
                Err(miette!(
                    "openai codex login requires the `oauth` feature; \
                     rebuild without `--no-default-features`"
                ))
            }
        }
        ProviderKind::Gemini => {
            let _ = (headless, codex_home);
            Err(miette!(
                "gemini does not have a PKCE flow wired yet. \
                 use the GEMINI_API_KEY env var, or wait until the gemini auth chain lands"
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

async fn cmd_status(
    provider: ProviderKind,
    codex_home: Option<PathBuf>,
) -> MietteResult<()> {
    let chain = build_chain(provider, codex_home).await?;

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
             run `pattern auth login {}` to authenticate",
            provider.as_str(),
            provider.as_str()
        )),
    }
}

// ---------------------------------------------------------------------------
// clear
// ---------------------------------------------------------------------------

async fn cmd_clear(
    provider: ProviderKind,
    codex_home: Option<PathBuf>,
) -> MietteResult<()> {
    #[cfg(feature = "oauth")]
    {
        match provider {
            ProviderKind::Anthropic | ProviderKind::Gemini => {
                if codex_home.is_some() {
                    eprintln!("note: --codex-home is ignored for {}", provider.as_str());
                }
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

                eprintln!(
                    "✓ cleared. next `auth login` falls through to session-pickup or PKCE."
                );
                Ok(())
            }
            ProviderKind::Openai => {
                let store = codex_store(codex_home)?;
                eprintln!(
                    "clearing codex stored credentials (keyring \"Codex Auth\" + {})",
                    store.auth_file_path().display()
                );
                store
                    .forget()
                    .await
                    .into_diagnostic()
                    .map_err(|e| miette!("clear failed: {e}"))?;
                eprintln!("✓ cleared. next `auth login openai` re-runs the OAuth flow.");
                Ok(())
            }
        }
    }
    #[cfg(not(feature = "oauth"))]
    {
        let _ = (provider, codex_home);
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
    codex_home: Option<PathBuf>,
) -> MietteResult<Arc<dyn CredentialChain>> {
    match provider {
        ProviderKind::Anthropic => {
            if codex_home.is_some() {
                eprintln!("note: --codex-home is ignored for anthropic");
            }
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
        ProviderKind::Openai => {
            #[cfg(feature = "oauth")]
            {
                let store = codex_store(codex_home)?;
                let chain: Arc<dyn CredentialChain> = Arc::new(OpenAiAuthChain::with_oauth(
                    Arc::new(store),
                    CodexOAuthConfig::codex(),
                    reqwest::Client::new(),
                ));
                Ok(chain)
            }
            #[cfg(not(feature = "oauth"))]
            {
                let _ = codex_home;
                let chain: Arc<dyn CredentialChain> = Arc::new(OpenAiAuthChain::api_key_only());
                Ok(chain)
            }
        }
        ProviderKind::Gemini => {
            if codex_home.is_some() {
                eprintln!("note: --codex-home is ignored for gemini");
            }
            let chain: Arc<dyn CredentialChain> = Arc::new(GeminiAuthChain::new());
            Ok(chain)
        }
    }
}

// ---------------------------------------------------------------------------
// Codex OAuth helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "oauth")]
fn codex_store(codex_home: Option<PathBuf>) -> MietteResult<CodexAuthStore> {
    match codex_home {
        Some(path) => Ok(CodexAuthStore::new(path)),
        None => CodexAuthStore::from_env().into_diagnostic(),
    }
}

#[cfg(feature = "oauth")]
async fn run_codex_login(flow: LoginFlow) -> MietteResult<CodexTokenSet> {
    let http = reqwest::Client::new();
    let handle = codex_begin_login(CodexOAuthConfig::codex(), flow, &http)
        .await
        .into_diagnostic()
        .map_err(|e| miette!("codex login could not start: {e}"))?;

    match &handle {
        CodexLoginHandle::Loopback(loopback) => {
            eprintln!();
            eprintln!("────────────────────────────────────────────────────────────");
            eprintln!("Opening your browser to authorize Pattern with OpenAI…");
            eprintln!();
            eprintln!("  {}", loopback.authorize_url);
            eprintln!();
            eprintln!("If the browser didn't open, copy that URL and visit it manually.");
            eprintln!("Pattern is waiting for the OAuth callback on localhost (≤ 5 min).");
            eprintln!("────────────────────────────────────────────────────────────");
            // Open is best-effort: failure prints a warning but doesn't
            // abort, since the user can still copy the URL manually.
            if let Err(e) = open::that_detached(&loopback.authorize_url) {
                eprintln!("⚠ could not open browser automatically: {e}");
                eprintln!("  copy the URL above and open it manually.");
            }
        }
        CodexLoginHandle::DeviceCode(dc) => {
            eprintln!();
            eprintln!("────────────────────────────────────────────────────────────");
            eprintln!("Device-code authorization");
            eprintln!();
            eprintln!("  1. Visit: {}", dc.verification_uri);
            if let Some(complete) = &dc.verification_uri_complete {
                eprintln!("     (or with the code pre-filled: {complete})");
            }
            eprintln!("  2. Enter this code:");
            eprintln!();
            eprintln!("        {}", dc.user_code);
            eprintln!();
            eprintln!("Pattern is polling for completion (expires in ~15 min).");
            eprintln!("────────────────────────────────────────────────────────────");
        }
    }

    codex_complete_login(handle, &http)
        .await
        .into_diagnostic()
        .map_err(|e| miette!("codex login did not complete: {e}"))
}

#[cfg(feature = "oauth")]
async fn persist_codex_token_set(
    store: &CodexAuthStore,
    token_set: &CodexTokenSet,
) -> MietteResult<()> {
    use pattern_provider::auth::{AuthDotJson, AuthMode, TokenData};

    // Pre-check whether the .auth.json file is already present so we
    // know whether to mirror to it. Pattern's rule: never *create* the
    // file, but mirror updates if codex CLI created it.
    let existing = store
        .load()
        .await
        .into_diagnostic()
        .map_err(|e| miette!("load existing codex store: {e}"))?;

    let auth = AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        // Preserve any pre-existing API-key field from a prior codex
        // login (e.g., the user previously ran `codex login --api-key`
        // and now also wants the OAuth path).
        openai_api_key: existing
            .auth
            .as_ref()
            .and_then(|a| a.openai_api_key.clone()),
        tokens: Some(TokenData {
            id_token: token_set.id_token.clone(),
            access_token: token_set.access_token.expose_secret().to_string(),
            refresh_token: token_set.refresh_token.expose_secret().to_string(),
            account_id: token_set.account_id.clone(),
        }),
        last_refresh: Some(jiff::Timestamp::now()),
        agent_identity: existing.auth.and_then(|a| a.agent_identity),
    };

    store
        .save(&auth, existing.file_existed)
        .await
        .into_diagnostic()
        .map_err(|e| miette!("persist codex token: {e}"))?;
    if existing.file_existed {
        eprintln!(
            "✓ stored in keyring (\"Codex Auth\") + {}",
            store.auth_file_path().display()
        );
    } else {
        eprintln!(
            "✓ stored in keyring (\"Codex Auth\"); {} not created (no existing file)",
            store.auth_file_path().display()
        );
    }
    Ok(())
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
