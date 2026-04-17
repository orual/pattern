//! `pattern-test-cli` — minimal live-tier verification tool.
//!
//! Exists to satisfy Phase 4 Task 11 (AC3.1 session-pickup, AC4.1 PKCE,
//! AC4.3 API key — all live-credential ACs that the plan explicitly
//! defers to a manual CLI checklist rather than env-gated test files)
//! and Task 20 (empirical `ShaperCompatMode` decision). Phase 5
//! extends this tool with a full runtime-backed smoke turn; Phase 4's
//! version is provider-only — it doesn't instantiate `TidepoolRuntime`.
//!
//! ## Commands
//!
//! - `auth`: resolve the per-provider credential chain and print which
//!   tier succeeded + sanitised credential metadata (token length, not
//!   token value; expiry; scope). Interactive PKCE: prints the
//!   authorize URL, waits for stdin paste of `code#state`, completes the
//!   exchange and stores the token.
//!
//! - `ask <prompt>`: build the gateway for the chosen provider, make a
//!   real streaming completion request, stream chunks to stdout, print
//!   an end-of-stream summary (usage, stop reason).
//!
//! ## Usage examples
//!
//! ```text
//! # AC3.1 — session pickup from ~/.claude/.credentials.json
//! pattern-test-cli auth --provider anthropic
//!
//! # AC4.1 — fresh PKCE flow (when session + key are both absent)
//! PATTERN_FORCE_PKCE=1 pattern-test-cli auth --provider anthropic
//!
//! # AC4.3 — API-key path
//! ANTHROPIC_API_KEY=sk-ant-... pattern-test-cli auth --provider anthropic
//!
//! # Task 20 — shaper mode verification
//! pattern-test-cli ask "hello" --shaper honest
//! pattern-test-cli ask "hello" --shaper subscription
//! ```
//!
//! This bin uses `pattern_provider` directly and does NOT construct a
//! `TidepoolRuntime`. That integration lands in Phase 5.

use std::io::Write;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use futures::StreamExt;
use pattern_core::traits::provider_client::ProviderClient;
use pattern_core::types::provider::{ChatMessage, ChatStreamEvent, CompletionRequest};
use pattern_provider::auth::{
    AnthropicAuthChain, CredentialChain, GeminiAuthChain, ResolvedCredential,
};
use pattern_provider::gateway::PatternGatewayClient;
use pattern_provider::ratelimit::ProviderRateLimiter;
use pattern_provider::shaper::{HonestPatternShaper, NoOpShaper, ShaperCompatMode, ShaperConfig};
use pattern_provider::token_count::TokenCounter;
use secrecy::ExposeSecret;

#[derive(Parser, Debug)]
#[command(
    name = "pattern-test-cli",
    about = "Live-tier verification tool for pattern_provider auth + gateway.",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Resolve the per-provider credential chain, print which tier won.
    /// Runs the interactive PKCE flow if it's reached and feature-enabled.
    Auth {
        #[arg(long, value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,
    },

    /// Make a streaming completion request against the provider.
    Ask {
        /// The user turn to send. Quote it if it contains shell metacharacters.
        prompt: String,

        #[arg(long, value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,

        /// Model identifier — passed verbatim to the gateway. Default
        /// fits Anthropic; override for Gemini etc.
        #[arg(long, default_value = "claude-opus-4-7")]
        model: String,

        /// Anthropic shaper mode. Ignored for non-Anthropic providers.
        #[arg(long, value_enum, default_value_t = ShaperMode::Default)]
        shaper: ShaperMode,

        /// Persona content injected into the shaper's persona slot.
        #[arg(long, default_value = "")]
        persona: String,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ProviderKind {
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

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ShaperMode {
    /// Use `ShaperCompatMode::default()` — whatever the feature-gated
    /// default currently is.
    Default,
    /// Force `HonestPattern` (single system block, no claude-code literal).
    Honest,
    /// Force `SubscriptionRoutingShape` (three-block structure).
    Subscription,
}

impl ShaperMode {
    fn resolve(self) -> ShaperCompatMode {
        match self {
            ShaperMode::Default => ShaperCompatMode::default(),
            ShaperMode::Honest => ShaperCompatMode::HonestPattern,
            #[cfg(feature = "subscription-oauth")]
            ShaperMode::Subscription => ShaperCompatMode::SubscriptionRoutingShape,
            #[cfg(not(feature = "subscription-oauth"))]
            ShaperMode::Subscription => {
                eprintln!("⚠ `--shaper subscription` requires the `subscription-oauth` feature");
                ShaperCompatMode::HonestPattern
            }
        }
    }
}

// ---- main ----

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,pattern_provider=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Auth { provider } => cmd_auth(provider).await,
        Cmd::Ask {
            prompt,
            provider,
            model,
            shaper,
            persona,
        } => cmd_ask(provider, model, prompt, shaper, persona).await,
    }
}

// ---- auth ----

async fn cmd_auth(provider: ProviderKind) -> Result<(), Box<dyn std::error::Error>> {
    let chain = build_chain(provider).await?;

    eprintln!("resolving credential chain for provider={}", provider.as_str());
    match chain.resolve().await {
        Ok(resolved) => {
            print_resolved(&resolved);
            Ok(())
        }
        #[cfg(feature = "subscription-oauth")]
        Err(pattern_core::error::ProviderError::NoAuthAvailable { .. })
            if matches!(provider, ProviderKind::Anthropic) =>
        {
            eprintln!("no credential resolved by any tier — starting PKCE flow");
            let token = run_pkce_interactive().await?;
            eprintln!("✓ PKCE flow completed");
            eprintln!("  tier: pkce (freshly obtained, not yet stored)");
            eprintln!("  access_token_len: {}", token.access_token.expose_secret().len());
            eprintln!("  refresh_token: {}", if token.refresh_token.is_some() { "present" } else { "absent" });
            eprintln!("  expires_at: {:?}", token.expires_at);
            eprintln!("  scope: {:?}", token.scope);
            Ok(())
        }
        Err(e) => {
            eprintln!("✗ chain resolution failed: {e}");
            Err(Box::new(e))
        }
    }
}

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

// ---- ask ----

async fn cmd_ask(
    provider: ProviderKind,
    model: String,
    prompt: String,
    shaper_mode: ShaperMode,
    persona: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let chain = build_chain(provider).await?;
    let limiter = Arc::new(match provider {
        ProviderKind::Anthropic => ProviderRateLimiter::anthropic_default(),
        ProviderKind::Gemini => ProviderRateLimiter::gemini_default(),
    });

    let mut gateway_builder = PatternGatewayClient::builder().with_persona(persona);

    match provider {
        ProviderKind::Anthropic => {
            let shaper_cfg = ShaperConfig {
                compat_mode: shaper_mode.resolve(),
                ..Default::default()
            };
            let shaper = Arc::new(HonestPatternShaper::new(shaper_cfg)?);
            let counter = Arc::new(TokenCounter::anthropic(limiter.clone()));
            gateway_builder = gateway_builder
                .with_provider("anthropic", chain, shaper, limiter)
                .with_token_counter("anthropic", counter);
        }
        ProviderKind::Gemini => {
            let shaper = Arc::new(NoOpShaper);
            gateway_builder = gateway_builder.with_provider("gemini", chain, shaper, limiter);
        }
    }

    let gateway = gateway_builder.build()?;

    let req = CompletionRequest::new(&model).append_message(ChatMessage::user(prompt));

    eprintln!("→ model={model} shaper={:?}", shaper_mode);
    let mut stream = gateway.complete(req).await?;
    let mut stdout = std::io::stdout().lock();
    let mut chunk_count = 0usize;
    let mut errors = 0usize;
    let mut saw_end = false;

    while let Some(evt) = stream.next().await {
        match evt {
            Ok(ChatStreamEvent::Chunk(c)) => {
                chunk_count += 1;
                stdout.write_all(c.content.as_bytes())?;
                stdout.flush()?;
            }
            Ok(ChatStreamEvent::ReasoningChunk(c)) => {
                // Reasoning separated to stderr so ask's stdout stays
                // pure-content.
                eprintln!("[reasoning] {}", c.content);
            }
            Ok(ChatStreamEvent::ToolCallChunk(_)) => {
                eprintln!("[tool-call chunk]");
            }
            Ok(ChatStreamEvent::End(end)) => {
                saw_end = true;
                writeln!(stdout)?;
                eprintln!(
                    "← end: chunks={chunk_count} usage={:?} reason={:?}",
                    end.captured_usage, end.captured_stop_reason,
                );
            }
            Ok(_) => {}
            Err(e) => {
                errors += 1;
                eprintln!("⚠ stream error: {e}");
            }
        }
    }

    if errors > 0 {
        std::process::exit(2);
    }
    if !saw_end {
        eprintln!("⚠ stream ended without End event");
        std::process::exit(3);
    }
    Ok(())
}

// ---- chain construction ----

async fn build_chain(
    provider: ProviderKind,
) -> Result<Arc<dyn CredentialChain>, Box<dyn std::error::Error>> {
    match provider {
        ProviderKind::Anthropic => {
            #[cfg(feature = "subscription-oauth")]
            {
                use pattern_provider::auth::{PkceTier, SessionPickupTier};
                use pattern_provider::creds_store::{
                    CredsStore, CredsStoreResolver, JsonFallbackStore, KeyringStore,
                };

                let session_pickup = SessionPickupTier::default();
                let pkce = Arc::new(PkceTier::anthropic());
                let primary: Arc<dyn CredsStore> = Arc::new(KeyringStore::new());
                let fallback: Arc<dyn CredsStore> = Arc::new(JsonFallbackStore::new()?);
                let creds_store: Arc<dyn CredsStore> =
                    Arc::new(CredsStoreResolver::new(primary, fallback));

                let chain: Arc<dyn CredentialChain> = Arc::new(
                    AnthropicAuthChain::with_oauth(session_pickup, pkce, creds_store),
                );
                Ok(chain)
            }
            #[cfg(not(feature = "subscription-oauth"))]
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

// ---- interactive PKCE ----

#[cfg(feature = "subscription-oauth")]
async fn run_pkce_interactive() -> Result<
    pattern_core::types::provider::ProviderCredential,
    Box<dyn std::error::Error>,
> {
    use pattern_provider::auth::PkceTier;
    use pattern_provider::creds_store::{
        CredsStore, CredsStoreResolver, JsonFallbackStore, KeyringStore,
    };

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
    std::io::stderr().flush()?;

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let pasted = line.trim();

    let token = tier.complete_manual(pending, pasted).await?;

    // Persist so subsequent `auth` / `ask` runs find the stored token
    // via the creds_store tier rather than re-running PKCE.
    let primary: Arc<dyn CredsStore> = Arc::new(KeyringStore::new());
    let fallback: Arc<dyn CredsStore> = Arc::new(JsonFallbackStore::new()?);
    let store = CredsStoreResolver::new(primary, fallback);
    if let Err(e) = store.put(&token).await {
        eprintln!("⚠ token obtained but store write failed: {e}");
        eprintln!("  (run `auth` again to retry storage; session-pickup path remains usable)");
    } else {
        eprintln!("✓ token stored via creds_store");
    }

    Ok(token)
}

#[cfg(not(feature = "subscription-oauth"))]
async fn run_pkce_interactive() -> Result<
    pattern_core::types::provider::ProviderCredential,
    Box<dyn std::error::Error>,
> {
    Err("PKCE flow requires the `subscription-oauth` feature".into())
}
