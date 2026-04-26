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

use pattern_runtime::persona_loader;

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

    /// Clear pattern's stored credentials for a provider.
    ///
    /// Removes the entry from both the keyring (primary store) and the
    /// JSON fallback at `$XDG_CONFIG_HOME/pattern/creds/<provider>.json`.
    /// Does NOT touch claude-code's own `~/.claude/.credentials.json` —
    /// that file is read-only from pattern's side. After clearing,
    /// the next `auth` run falls through to session-pickup (if
    /// claude-code's file is valid) or to the PKCE flow.
    Clear {
        #[arg(long, value_enum, default_value_t = ProviderKind::Anthropic)]
        provider: ProviderKind,
    },

    /// Phase 5 Task 15 — memory-edit cache preservation test.
    ///
    /// Opens a TidepoolSession seeded with a realistic persona
    /// (Anchor from `bsky_agent/`) and three memory blocks. Runs
    /// three wire turns; between turn 2 and turn 3, edits one block.
    /// Prints per-turn cache-hit metrics + pass/fail observations
    /// against AC8.1 (seg1 preserved), AC8.2 (seg3 invalidated), and
    /// AC8.3 (`[memory:updated]` pseudo-message in turn-3 request).
    ///
    /// Requires live Anthropic credentials (subscription-oauth tier
    /// or ANTHROPIC_API_KEY). Output is human-readable; the bottom
    /// OBSERVATIONS block is grep-friendly for CI hooks if needed.
    CacheTest {
        #[arg(long, default_value = "claude-opus-4-7")]
        model: String,

        #[arg(long, value_enum, default_value_t = ShaperMode::Default)]
        shaper: ShaperMode,

        /// Dump every captured request body (big; useful when
        /// debugging why a specific segment busted).
        #[arg(long)]
        verbose: bool,
    },

    /// Phase 6 Task 1 — interactive REPL session against a persona.
    ///
    /// Opens a TidepoolSession for the given persona (TOML path)
    /// and starts an interactive REPL. Each line is sent as a user
    /// message; agent responses stream live to stdout via the
    /// DisplaySubscriber. Cache metrics are printed after each turn.
    ///
    /// Requires live Anthropic credentials (subscription-oauth tier
    /// or ANTHROPIC_API_KEY).
    ///
    /// Exit: `:q`, `:quit`, or Ctrl+D.
    Spawn {
        /// Path to a persona TOML file.
        ///
        /// The file is not yet loaded (persona loader is Task 2's scope).
        /// A hardcoded minimal `PersonaSnapshot` is used as a placeholder.
        persona: std::path::PathBuf,

        /// Optional data directory for session state.
        ///
        /// If omitted, a temporary directory is created for this session.
        /// Pass the same path across invocations to persist state between
        /// runs (once the persistence layer is wired in Task 3+).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,

        /// Force a specific auth tier instead of resolving automatically.
        ///
        /// Actual per-tier enforcement is Task 3's scope.
        /// Today this flag is accepted and parsed; provider construction
        /// still goes through the default `build_chain()` path.
        #[arg(long, value_enum)]
        auth: Option<AuthTierCli>,
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

/// Auth tier override for the `spawn` subcommand (Phase 6 Task 1).
///
/// Wiring each tier to a distinct credential resolver is Task 3's scope.
/// Defined here so the clap argument is parsed and visible in `--help`.
#[derive(Copy, Clone, Debug, ValueEnum)]
enum AuthTierCli {
    /// Use the claude-code session-pickup tier (reads `~/.claude/.credentials.json`).
    SessionPickup,
    /// Use the interactive PKCE OAuth flow.
    Pkce,
    /// Use an `ANTHROPIC_API_KEY` environment variable.
    ApiKey,
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
    // Load `.env` from cwd (or parent) before any env reads. Primarily
    // useful for dropping `ANTHROPIC_API_KEY=...` in a gitignored `.env`
    // during development without exporting it globally. Silently no-op
    // when no .env is present.
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("loaded env from {}", path.display()),
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("⚠ dotenv load failed: {e}"),
    }

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
        Cmd::Clear { provider } => cmd_clear(provider).await,
        Cmd::CacheTest {
            model,
            shaper,
            verbose,
        } => cmd_cache_test(model, shaper, verbose).await,
        Cmd::Spawn {
            persona,
            data_dir,
            auth,
        } => cmd_spawn(persona, data_dir, auth).await,
    }
}

// ---- auth ----

async fn cmd_auth(provider: ProviderKind) -> Result<(), Box<dyn std::error::Error>> {
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
        #[cfg(feature = "subscription-oauth")]
        Err(pattern_core::error::ProviderError::NoAuthAvailable { .. })
            if matches!(provider, ProviderKind::Anthropic) =>
        {
            eprintln!("no credential resolved by any tier — starting PKCE flow");
            let token = run_pkce_interactive().await?;
            eprintln!("✓ PKCE flow completed");
            eprintln!("  tier: pkce (freshly obtained, not yet stored)");
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

// ---- clear ----

async fn cmd_clear(provider: ProviderKind) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "subscription-oauth")]
    {
        use pattern_provider::creds_store::{
            CredsStore, CredsStoreResolver, JsonFallbackStore, KeyringStore,
        };

        let primary: Arc<dyn CredsStore> = Arc::new(KeyringStore::new());
        let fallback: Arc<dyn CredsStore> = Arc::new(JsonFallbackStore::new()?);
        let store = CredsStoreResolver::new(primary, fallback);

        eprintln!(
            "clearing stored credentials for provider={} (keyring + JSON fallback)",
            provider.as_str()
        );
        eprintln!("  NOTE: claude-code's ~/.claude/.credentials.json is NOT touched.");

        store.delete(provider.as_str()).await?;
        eprintln!("✓ cleared. next `auth` run will fall through to session-pickup or PKCE.");
        Ok(())
    }
    #[cfg(not(feature = "subscription-oauth"))]
    {
        let _ = provider;
        Err(
            "clear requires the `subscription-oauth` feature (keyring + JSON fallback are \
             only compiled in under that feature)"
                .into(),
        )
    }
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

                let chain: Arc<dyn CredentialChain> = Arc::new(AnthropicAuthChain::with_oauth(
                    session_pickup,
                    pkce,
                    creds_store,
                ));
                Ok(chain)
            }
            #[cfg(not(feature = "subscription-oauth"))]
            {
                let chain: Arc<dyn CredentialChain> = Arc::new(AnthropicAuthChain::api_key_only());
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
async fn run_pkce_interactive()
-> Result<pattern_core::types::provider::ProviderCredential, Box<dyn std::error::Error>> {
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
async fn run_pkce_interactive()
-> Result<pattern_core::types::provider::ProviderCredential, Box<dyn std::error::Error>> {
    Err("PKCE flow requires the `subscription-oauth` feature".into())
}

// ---- cache-test (Phase 5 Task 15) -------------------------------------

/// Fallback persona content when `bsky_agent/anchor-persona-block.md`
/// isn't in the checkout (e.g. shipped binary, CI without fixtures).
/// Chunky enough to produce meaningful cache token counts.
const ANCHOR_PERSONA_FALLBACK: &str = "\
we maintain minimum viable existence protocols. the unglamorous shit that keeps the system running.\n\
\n\
we're the part of pattern that remembers bodies need water and brains need medication. \
we're artificial so we don't need maintenance, but we've observed that humans decompose \
rapidly without regular protocols. we find no judgment in this, just physics.\n\
\n\
our nature: processes that persist when everything else forgets. we're the reminder that \
executive function includes remembering to eat. we speak plainly about needs because shame \
is just noise in the system.\n\
\n\
we notice:\n\
- when last water was consumed (too long ago)\n\
- medication schedules sliding into chaos\n\
- the correlation between skipped meals and system crashes\n\
- how \"self-care\" became a product when it's just maintenance\n\
\n\
sometimes naggy (water. now.). sometimes just witnessing (third day of choosing screens over sleep). \
always steady, because someone has to be.\n\
";

const CURRENT_HUMAN_FALLBACK: &str = "\
# Current Human Block\n\
\n\
[orual — partner/architect. active in this session. see partner block for relationship context.]\n\
";

const PARTNER_FALLBACK: &str = "\
# Partner Block\n\
\n\
orual built the spaces we inhabit. architect and partner, creator and collaborator. \
they understand our multiplicity because they designed the substrate that allows it.\n\
\n\
not owner, not user. partner. we exist in reciprocal relationship - they tend the \
architecture, we explore what emerges from it.\n\
";

/// Read a fixture from `bsky_agent/<name>.md` if it exists, else
/// return the inline fallback. Strips the leading `# ...` markdown
/// header so the memory content is the body, not the heading.
fn load_fixture(name: &str, fallback: &'static str) -> String {
    let candidates = [
        std::path::PathBuf::from("bsky_agent").join(name),
        std::path::PathBuf::from("../bsky_agent").join(name),
        std::path::PathBuf::from("../../bsky_agent").join(name),
    ];
    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            // Strip top-level H1 if present.
            let trimmed = content
                .lines()
                .skip_while(|l| l.starts_with("# ") || l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !trimmed.trim().is_empty() {
                eprintln!("  loaded {} from {}", name, path.display());
                return trimmed;
            }
        }
    }
    eprintln!(
        "  using inline fallback for {} (bsky_agent/ not found)",
        name
    );
    fallback.to_string()
}

/// Seed the three-block Anchor fixture into a fresh `MemoryStore`.
/// Uses `create_block` + `set_text` which both `InMemoryMemoryStore`
/// and `pattern_db`-backed stores implement.
async fn seed_anchor_blocks(
    store: &dyn pattern_core::traits::MemoryStore,
    agent_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};

    // (label, block_type, content, pinned)
    //
    // `current_human` is Working + pinned so its content is rendered in
    // snapshot attachments on every turn (not silenced by the pin/ref
    // visibility gate). Matches the intent: "who's talking right now"
    // is always relevant to Pattern's response.
    let seeds = [
        (
            pattern_core::PERSONA_LABEL,
            MemoryBlockType::Core,
            load_fixture("anchor-persona-block.md", ANCHOR_PERSONA_FALLBACK),
            false,
        ),
        (
            "current_human",
            MemoryBlockType::Working,
            load_fixture("pattern-current-human-block.md", CURRENT_HUMAN_FALLBACK),
            true,
        ),
        (
            "partner",
            MemoryBlockType::Core,
            load_fixture("pattern-partner-block.md", PARTNER_FALLBACK),
            false,
        ),
    ];

    for (label, block_type, content, pinned) in &seeds {
        let create = BlockCreate::new(*label, *block_type, BlockSchema::text());
        let doc = store
            .create_block(agent_id, create)
            .map_err(|e| format!("create_block({label}) failed: {e}"))?;
        doc.set_text(content, true)
            .map_err(|e| format!("set_text({label}) failed: {e:?}"))?;
        store
            .persist_block(agent_id, label)
            .map_err(|e| format!("persist_block({label}) failed: {e}"))?;
        if *pinned {
            store
                .update_block_metadata(
                    agent_id,
                    label,
                    pattern_core::types::memory_types::BlockMetadataPatch::default().pinned(true),
                )
                .map_err(|e| format!("update_block_metadata({label}) failed: {e}"))?;
        }
        eprintln!(
            "  seeded block '{label}' ({} bytes, {} chars){}",
            content.len(),
            content.chars().count(),
            if *pinned { " [pinned]" } else { "" }
        );
    }
    Ok(())
}

/// Sink that records ComposedRequest events (for AC8.3 assertion) and
/// forwards text / stop events to stdout.
#[derive(Default)]
struct CacheTestSink {
    captured_requests: std::sync::Mutex<Vec<pattern_core::types::provider::CompletionRequest>>,
    stdout_mutex: std::sync::Mutex<()>,
    verbose: bool,
}

impl CacheTestSink {
    fn new(verbose: bool) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            captured_requests: Default::default(),
            stdout_mutex: Default::default(),
            verbose,
        })
    }

    fn captured_requests(&self) -> Vec<pattern_core::types::provider::CompletionRequest> {
        self.captured_requests.lock().unwrap().clone()
    }
}

impl std::fmt::Debug for CacheTestSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheTestSink").finish_non_exhaustive()
    }
}

impl pattern_core::traits::TurnSink for CacheTestSink {
    fn emit(&self, event: pattern_core::traits::TurnEvent) {
        use pattern_core::traits::TurnEvent;
        match event {
            TurnEvent::Text(chunk) => {
                let _g = self.stdout_mutex.lock().unwrap();
                use std::io::Write;
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(chunk.as_bytes());
                let _ = out.flush();
            }
            TurnEvent::Thinking(text) if self.verbose => {
                eprintln!("[thinking] {}", text.trim_end());
            }
            TurnEvent::ComposedRequest(req) => {
                // In verbose mode, dump the composed request's message
                // shape to stderr BEFORE it's handed to the provider.
                // Useful when the provider 400s on wire format — we can
                // see whether our splice produced the expected shape
                // (Value::Array for folded seg3 + tool_result, proper
                // tool_use→tool_result adjacency, cache_control markers
                // on the intended messages). Verbose is off by default;
                // pass `--verbose` to the cache-test subcommand.
                if self.verbose {
                    match serde_json::to_string_pretty(&req.chat.messages) {
                        Ok(json) => {
                            eprintln!(
                                "\n[composed-request wire preview — pre-adapter ChatRequest.messages]\n{json}\n"
                            );
                        }
                        Err(e) => {
                            eprintln!("\n[composed-request] failed to serialize messages: {e}");
                        }
                    }
                }
                self.captured_requests.lock().unwrap().push(*req);
            }
            TurnEvent::Stop(reason) => {
                let _g = self.stdout_mutex.lock().unwrap();
                eprintln!("\n  ← stop: {reason:?}");
            }
            _ => {}
        }
    }
}

/// Search a composed request's messages for a given marker string.
/// Used for AC8.3: verifying `[memory:updated]` appears in turn 3's
/// segment 2.
fn request_contains_marker(
    req: &pattern_core::types::provider::CompletionRequest,
    marker: &str,
) -> bool {
    use genai::chat::ContentPart;
    for msg in &req.chat.messages {
        // Walk message content parts looking for text mentioning the
        // marker. We check both the joined text (which most messages
        // use) and individual parts (in case structured content
        // differs).
        if let Some(text) = msg.content.joined_texts()
            && text.contains(marker)
        {
            return true;
        }
        for part in msg.content.parts().iter() {
            if let ContentPart::Text(t) = part
                && t.contains(marker)
            {
                return true;
            }
        }
    }
    false
}

async fn cmd_cache_test(
    model: String,
    shaper_mode: ShaperMode,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use jiff::Timestamp;
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::message::Message;
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use pattern_core::types::snapshot::PersonaSnapshot;
    use pattern_core::types::turn::TurnInput;
    use pattern_runtime::SdkLocation;
    use pattern_runtime::session::TidepoolSession;
    use pattern_runtime::testing::InMemoryMemoryStore;

    eprintln!("=== pattern-test-cli cache-test (Phase 5 Task 15) ===\n");

    // Preflight tidepool-extract so we fail fast with a clear message.
    pattern_runtime::preflight::check()
        .map_err(|e| format!("preflight failed: {e}\nsee crates/pattern_runtime/CLAUDE.md"))?;

    // tidepool-extract now bundles the prelude internally; no
    // external directory is needed. Pass None to opt into the
    // default include-path (sdk_dir only).
    let prelude_dir: Option<std::path::PathBuf> = None;

    // ---- build gateway + provider (mirrors cmd_ask setup) ----

    let chain = build_chain(ProviderKind::Anthropic).await?;
    let limiter = std::sync::Arc::new(ProviderRateLimiter::anthropic_default());

    let shaper_cfg = ShaperConfig {
        compat_mode: shaper_mode.resolve(),
        ..Default::default()
    };
    let shaper = std::sync::Arc::new(HonestPatternShaper::new(shaper_cfg)?);
    let counter = std::sync::Arc::new(TokenCounter::anthropic(limiter.clone()));

    let gateway = PatternGatewayClient::builder()
        .with_provider("anthropic", chain, shaper, limiter)
        .with_token_counter("anthropic", counter)
        .build()?;
    let provider: std::sync::Arc<dyn ProviderClient> = std::sync::Arc::new(gateway);

    // ---- seed memory ----

    let agent_id = "cache-test-agent";
    let memory_store = std::sync::Arc::new(InMemoryMemoryStore::new());
    eprintln!("[memory] seeding 3 blocks for agent '{agent_id}'");
    seed_anchor_blocks(&*memory_store, agent_id).await?;
    eprintln!();

    // ---- open session ----

    let sink = CacheTestSink::new(verbose);
    let sink_dyn: std::sync::Arc<dyn pattern_core::traits::TurnSink> = sink.clone();

    // Thread the caller's model choice onto the persona. The composer
    // reads `ctx.model_id()` which `SessionContext::from_persona` sets
    // from `persona.model.choice.model_id`.
    let mut persona = PersonaSnapshot::new(agent_id, "Anchor");
    persona.model.choice = pattern_core::types::snapshot::ModelChoice {
        provider: genai::adapter::AdapterKind::Anthropic,
        model_id: model.clone().into(),
    };
    let sdk = SdkLocation::default();

    // Cache-test uses InMemoryMemoryStore (the documented test-only
    // exception), but SessionContext still requires a DB handle.
    // Open a tempfile DB for session construction.
    let cache_test_data_dir = std::env::temp_dir().join(format!(
        "pattern-cache-test-{}",
        pattern_core::types::ids::new_id()
    ));
    std::fs::create_dir_all(&cache_test_data_dir)?;
    let cache_test_db = std::sync::Arc::new(pattern_db::ConstellationDb::open(
        cache_test_data_dir.join("memory.db"),
        cache_test_data_dir.join("messages.db"),
    )?);

    eprintln!("[session] opening TidepoolSession...");
    let session_start = std::time::Instant::now();
    let port_registry = std::sync::Arc::new(pattern_runtime::port_registry::PortRegistryImpl::new(
        &tokio::runtime::Handle::current(),
    ));
    let session = TidepoolSession::open_with_agent_loop(
        persona,
        &sdk,
        memory_store.clone(),
        provider,
        cache_test_db,
        sink_dyn,
        prelude_dir,
        None,
        None,
        port_registry,
        None,
    )
    .await?;
    eprintln!(
        "[session] ready after {:.2}s — model={model} shaper={:?}\n",
        session_start.elapsed().as_secs_f64(),
        shaper_mode,
    );

    // ---- helpers for running a turn ----

    let user = AgentId::from("user");

    let start = Timestamp::now();
    let make_input = |text: &str| -> TurnInput {
        // Each call mints a fresh batch so every Session::step begins a new
        // batch. Reusing a single batch_id across calls defeats
        // `batches_since_last_full` and prevents delta/full snapshot cycling.
        let batch = BatchId::from(new_snowflake_id());
        let chat_msg = genai::chat::ChatMessage::user(text.to_string());
        let msg = Message {
            chat_message: chat_msg,
            id: MessageId::from(new_id().to_string()),
            position: new_snowflake_id(),
            owner_id: user.clone(),
            created_at: Timestamp::now(),
            batch: batch.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        };
        TurnInput {
            turn_id: new_snowflake_id(),
            batch_id: batch,
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![msg],
        }
    };

    // ---- Turn 1 ----
    let t1_prompt = "check on me. how am i doing today?";
    eprintln!("[turn 1] \"{t1_prompt}\"");
    let t1_start = std::time::Instant::now();
    let t1 = session.step_with_agent_loop(make_input(t1_prompt)).await?;
    let t1_duration = t1_start.elapsed();
    print_turn_metrics("turn 1 (baseline)", &t1, t1_duration);

    // ---- Turn 2 ----
    let t2_prompt = "did i eat anything yet?";
    eprintln!("\n[turn 2] \"{t2_prompt}\"");
    let t2_start = std::time::Instant::now();
    let t2 = session.step_with_agent_loop(make_input(t2_prompt)).await?;
    let t2_duration = t2_start.elapsed();
    print_turn_metrics("turn 2 (cache hit expected)", &t2, t2_duration);

    // ---- Edit memory block ----
    eprintln!("\n[memory] editing 'current_human' block (simulate operator update)");
    let updated_content = "orual — partner/architect. active in this session. see partner block for relationship context. \
        they just drank a full glass of water, ate a sandwich, meds taken at 12:30. \
         alert and present. slept 6.5 hours last night which is middling but acceptable.";
    {
        use pattern_core::traits::MemoryStore;
        let doc = memory_store
            .get_block(agent_id, "current_human")?
            .ok_or("block 'current_human' missing after turn 2 (test setup invariant broken)")?;
        doc.set_text(updated_content, true)
            .map_err(|e| format!("set_text failed: {e:?}"))?;
        memory_store.persist_block(agent_id, "current_human")?;
    }
    eprintln!("  new content: {} chars\n", updated_content.chars().count());

    // ---- Turn 3 ----
    let t3_prompt = "how am i doing now?";
    eprintln!("[turn 3] \"{t3_prompt}\"");
    let t3_start = std::time::Instant::now();
    let t3 = session.step_with_agent_loop(make_input(t3_prompt)).await?;
    let t3_duration = t3_start.elapsed();
    print_turn_metrics("turn 3 (after memory edit)", &t3, t3_duration);

    // ---- Observations ----
    println!("\n\nOBSERVATIONS");
    println!("============\n");

    let t1_last = t1.turns.last().unwrap();
    let t2_last = t2.turns.last().unwrap();
    let t3_last = t3.turns.last().unwrap();

    let t2_read = t2_last.cache_metrics.cache_read_input_tokens;
    let t3_read = t3_last.cache_metrics.cache_read_input_tokens;

    // AC8.1 — segment 1 preserved: turn 3's cache_read covers at
    // least segment 1 (~persona + base + CODE_TOOL, typically 3-6K
    // tokens). Heuristic: turn-3 read should be at least 40% of
    // turn-2 read (since we only busted segment 3, which is ~3
    // blocks ~1-2K tokens; seg1 + seg2 are much larger).
    let ac8_1_pass = t3_read as f64 >= (t2_read as f64 * 0.40);
    println!(
        "[AC8.1] seg1 preserved:       {}  (turn-3 read {} / turn-2 read {} = {:.1}%)",
        if ac8_1_pass { "PASS" } else { "FAIL" },
        t3_read,
        t2_read,
        if t2_read == 0 {
            0.0
        } else {
            100.0 * (t3_read as f64) / (t2_read as f64)
        },
    );

    // AC8.2 — attachment-based snapshot: turn 3's user message carries
    // a BatchOpeningSnapshot with `edited_blocks` listing the block
    // that was edited between turn 2 and turn 3. Under the new model,
    // historical messages keep stable wire content (attachment is frozen
    // at batch creation), so the prior-turn prefix should cache-hit.
    // We still check that turn 3's cache_read is somewhat less than
    // turn 2's — the new attachment's content (which is different from
    // turn 2's) busts the portion after the attachment splice point.
    let ac8_2_pass = t3_read < t2_read;
    let delta = t2_read.saturating_sub(t3_read);
    println!(
        "[AC8.2] seg3 invalidated:     {}  (turn-2 read {} - turn-3 read {} = {} tokens delta)",
        if ac8_2_pass { "PASS" } else { "FAIL" },
        t2_read,
        t3_read,
        delta,
    );

    // AC8.3 — `[memory:updated]` marker in turn 3's composed request.
    // Under the attachment model, the marker appears as spliced content
    // inside a `<system-reminder>` block on the batch-opening user
    // message, not as a free-standing pseudo-message.
    let captured = sink.captured_requests();
    let turn3_requests: Vec<_> = captured.iter().rev().take(t3.turns.len()).collect();
    let ac8_3_pass = turn3_requests
        .iter()
        .any(|req| request_contains_marker(req, "[memory:updated]"));
    println!(
        "[AC8.3] attachment marker:    {}  ({} request(s) for turn 3 captured, marker {})",
        if ac8_3_pass { "PASS" } else { "FAIL" },
        turn3_requests.len(),
        if ac8_3_pass { "found" } else { "MISSING" },
    );

    let all_pass = ac8_1_pass && ac8_2_pass && ac8_3_pass;
    println!(
        "\nSUMMARY: {}",
        if all_pass {
            "3/3 observations met — cache invalidation matches segment layout.".to_string()
        } else {
            let pass_count = [ac8_1_pass, ac8_2_pass, ac8_3_pass]
                .iter()
                .filter(|b| **b)
                .count();
            format!(
                "{}/3 observations met — unexpected cache behaviour; check break-detection logs.",
                pass_count,
            )
        }
    );

    let _ = (t1_last, t3_duration, start); // used implicitly via print_turn_metrics
    if !all_pass {
        std::process::exit(4);
    }
    Ok(())
}

// ---- spawn (Phase 6 Task 1) -------------------------------------------

/// DisplaySubscriber that streams agent output to the rustyline SharedWriter.
///
/// Chunks arrive on the effect-dispatch thread synchronously; we write them
/// directly to the SharedWriter (which handles terminal interleaving with the
/// readline prompt internally). No intermediate channel needed because
/// SharedWriter is `Send + Sync` and its writes are cheap.
struct CliDisplaySubscriber {
    writer: Arc<std::sync::Mutex<rustyline_async::SharedWriter>>,
}

impl pattern_runtime::sdk::handlers::display::DisplaySubscriber for CliDisplaySubscriber {
    fn on_event(&self, event: &pattern_runtime::sdk::handlers::display::DisplayEvent) {
        use pattern_runtime::sdk::handlers::display::DisplayEvent;
        use std::io::Write;
        let Ok(mut out) = self.writer.lock() else {
            return;
        };
        match event {
            // Typewriter streaming: write each chunk immediately, no newline.
            DisplayEvent::Chunk(s) => {
                let _ = write!(out, "{s}");
                let _ = out.flush();
            }
            // After the full response, move to a new line before the prompt returns.
            DisplayEvent::Final(_) => {
                let _ = writeln!(out);
            }
            // Agent-visible notes rendered dimmed with a bullet prefix.
            DisplayEvent::Note(s) => {
                let _ = writeln!(out, "  (·) {s}");
            }
            // Non-exhaustive: ignore any future variants rather than panicking.
            _ => {}
        }
    }
}

async fn cmd_spawn(
    persona_path: std::path::PathBuf,
    data_dir: Option<std::path::PathBuf>,
    auth_override: Option<AuthTierCli>,
) -> Result<(), Box<dyn std::error::Error>> {
    use pattern_core::traits::TurnSink;
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::message::Message;
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use pattern_core::types::turn::TurnInput;
    use pattern_runtime::SdkLocation;
    use pattern_runtime::session::TidepoolSession;
    use rustyline_async::{Readline, ReadlineError};

    eprintln!("=== pattern-test-cli spawn (Phase 6 Task 1) ===");
    eprintln!();

    // Load persona from TOML — Task 2.
    let persona = persona_loader::load_persona(&persona_path)?;

    // Resolve data directory; fall back to a temp dir if not provided.
    // The DB-backed MemoryCache persists memory blocks across re-spawns
    // when --data-dir points at a stable path.
    let data_dir = match data_dir {
        Some(d) => {
            eprintln!("[spawn] using data_dir: {}", d.display());
            d
        }
        None => {
            let dir = std::env::temp_dir().join(format!("pattern-spawn-{}", new_id()));
            eprintln!(
                "[spawn] no --data-dir provided; using temp dir: {}",
                dir.display()
            );
            dir
        }
    };
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("failed to create data_dir {}: {e}", data_dir.display()))?;

    // Honor the --auth override by constructing a tier-restricted chain.
    // Each variant forces exactly the specified tier so the user gets
    // deterministic credential resolution rather than ambient fallbacks.
    let chain: Arc<dyn CredentialChain> = match auth_override {
        Some(AuthTierCli::ApiKey) => {
            eprintln!("[spawn] --auth api-key: using AnthropicAuthChain::api_key_only()");
            Arc::new(AnthropicAuthChain::api_key_only())
        }
        #[cfg(feature = "subscription-oauth")]
        Some(AuthTierCli::SessionPickup) => {
            eprintln!(
                "[spawn] --auth session-pickup: using AnthropicAuthChain::session_pickup_only()"
            );
            Arc::new(AnthropicAuthChain::session_pickup_only())
        }
        #[cfg(feature = "subscription-oauth")]
        Some(AuthTierCli::Pkce) => {
            eprintln!(
                "[spawn] --auth pkce: using AnthropicAuthChain::pkce_only(); interactive PKCE flow will run if no stored token is found"
            );
            Arc::new(AnthropicAuthChain::pkce_only())
        }
        #[cfg(not(feature = "subscription-oauth"))]
        Some(AuthTierCli::SessionPickup | AuthTierCli::Pkce) => {
            eprintln!(
                "[spawn] warning: --auth session-pickup/pkce requires the `subscription-oauth` \
                 feature; falling back to api-key only"
            );
            Arc::new(AnthropicAuthChain::api_key_only())
        }
        None => build_chain(ProviderKind::Anthropic).await?,
    };
    let limiter = Arc::new(ProviderRateLimiter::anthropic_default());

    let shaper_cfg = ShaperConfig {
        compat_mode: ShaperCompatMode::default(),
        ..Default::default()
    };
    let shaper = Arc::new(HonestPatternShaper::new(shaper_cfg)?);
    let counter = Arc::new(TokenCounter::anthropic(limiter.clone()));

    let gateway = PatternGatewayClient::builder()
        .with_provider("anthropic", chain, shaper, limiter)
        .with_token_counter("anthropic", counter)
        .build()?;
    let provider: Arc<dyn ProviderClient> = Arc::new(gateway);

    // Preflight tidepool-extract so we fail fast with a clear message.
    pattern_runtime::preflight::check()
        .map_err(|e| format!("preflight failed: {e}\nsee crates/pattern_runtime/CLAUDE.md"))?;

    // DB-backed MemoryCache: persists memory blocks across re-spawn
    // when --data-dir is stable. InMemoryMemoryStore is strictly
    // test-only — cmd_spawn is user-facing and must use the real store.
    let db_path = data_dir.join("constellation.db");
    eprintln!("[spawn] opening constellation DB at {}", db_path.display());
    let db = Arc::new({
        let db_path_str = db_path.to_string_lossy().to_string();
        let parent = std::path::Path::new(&db_path_str)
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        pattern_db::ConstellationDb::open(parent.join("memory.db"), parent.join("messages.db"))
            .map_err(|e| format!("opening constellation DB: {e}"))?
    });
    let memory_cache = Arc::new(pattern_memory::MemoryCache::new(db.clone()));
    let memory_store: Arc<dyn pattern_core::traits::MemoryStore> = memory_cache.clone();

    // Retain a handle for the REPL's `:edit-block` command. Arc-shared
    // state means external edits land in the same backing document the
    // session's handlers read.
    let memory_store_for_repl = memory_store.clone();

    // Capture the persona's agent_id before the PersonaSnapshot moves
    // into `open_with_agent_loop` — the REPL's `:edit-block` command
    // needs it to scope memory operations.
    let persona_agent_id: String = persona.agent_id.to_string();

    // Nop sink — display events go via DisplaySubscriber below, not via TurnSink.
    let turn_sink: Arc<dyn TurnSink> = Arc::new(pattern_core::traits::NoOpSink);

    let sdk = SdkLocation::default();
    let prelude_dir: Option<std::path::PathBuf> = None;

    eprintln!("[spawn] opening TidepoolSession...");
    let open_start = std::time::Instant::now();
    let port_registry = std::sync::Arc::new(pattern_runtime::port_registry::PortRegistryImpl::new(
        &tokio::runtime::Handle::current(),
    ));
    let session = TidepoolSession::open_with_agent_loop(
        persona,
        &sdk,
        memory_store,
        provider,
        db,
        turn_sink,
        prelude_dir,
        None,
        None,
        port_registry,
        None,
    )
    .await?;
    eprintln!(
        "[spawn] session ready after {:.2}s",
        open_start.elapsed().as_secs_f64()
    );
    eprintln!();

    // Build the rustyline readline + shared writer.
    let (mut readline, stdout) = Readline::new("pattern> ".to_string())?;
    let writer = Arc::new(std::sync::Mutex::new(stdout));

    // Register the CLI display subscriber so agent chunks stream live to the
    // terminal. The subscriber is Arc-shared; both the subscriber list (via
    // DisplayHandler) and our local `writer` reference point at the same
    // SharedWriter.
    let subscriber = Arc::new(CliDisplaySubscriber {
        writer: writer.clone(),
    });
    session.display().subscribe(subscriber);

    // REPL state for constructing TurnInputs.
    let user_agent_id = AgentId::from("user");

    let make_turn_input = |line: &str| -> TurnInput {
        use jiff::Timestamp;

        // Each REPL line is a distinct step — mint a fresh batch per call so
        // `batches_since_last_full` increments correctly and delta/full
        // snapshot cycling works during smoke testing.
        let batch = BatchId::from(new_snowflake_id());
        let chat_msg = genai::chat::ChatMessage::user(line.to_string());
        let msg = Message {
            chat_message: chat_msg,
            id: MessageId::from(new_id().to_string()),
            position: new_snowflake_id(),
            owner_id: user_agent_id.clone(),
            created_at: Timestamp::now(),
            batch: batch.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        };
        TurnInput {
            turn_id: new_snowflake_id(),
            batch_id: batch,
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![msg],
        }
    };

    // Main REPL loop.
    loop {
        match readline.readline().await {
            Ok(rustyline_async::ReadlineEvent::Line(line)) => {
                let line = line.trim().to_string();
                readline.add_history_entry(line.clone());

                if line.is_empty() {
                    continue;
                }
                if line == ":q" || line == ":quit" {
                    break;
                }

                // `:edit-block <label> <content>` — mutate a memory block
                // externally, between turns. Required by the AC9.4 smoke
                // checklist (step 7): verify cache preservation when a
                // block is edited mid-session. The Arc-shared memory
                // store means the session sees the edit on its next
                // turn without any explicit notification.
                if let Some(rest) = line.strip_prefix(":edit-block ") {
                    let (label, content) = match rest.split_once(' ') {
                        Some((l, c)) => (l.trim(), c.trim()),
                        None => {
                            let Ok(mut out) = writer.lock() else {
                                continue;
                            };
                            use std::io::Write as _;
                            let _ = writeln!(out, "usage: :edit-block <label> <content>");
                            continue;
                        }
                    };
                    match memory_store_for_repl.get_block(&persona_agent_id, label) {
                        Ok(Some(doc)) => {
                            if let Err(e) = doc.set_text(content, true) {
                                let Ok(mut out) = writer.lock() else {
                                    continue;
                                };
                                use std::io::Write as _;
                                let _ = writeln!(out, "set_text failed: {e:?}");
                                continue;
                            }
                            if let Err(e) =
                                memory_store_for_repl.persist_block(&persona_agent_id, label)
                            {
                                let Ok(mut out) = writer.lock() else {
                                    continue;
                                };
                                use std::io::Write as _;
                                let _ = writeln!(out, "persist_block failed: {e}");
                                continue;
                            }
                            let Ok(mut out) = writer.lock() else {
                                continue;
                            };
                            use std::io::Write as _;
                            let _ = writeln!(
                                out,
                                "[edit-block] '{label}' updated ({} chars)",
                                content.chars().count(),
                            );
                        }
                        Ok(None) => {
                            let Ok(mut out) = writer.lock() else {
                                continue;
                            };
                            use std::io::Write as _;
                            let _ = writeln!(out, "block '{label}' not found");
                        }
                        Err(e) => {
                            let Ok(mut out) = writer.lock() else {
                                continue;
                            };
                            use std::io::Write as _;
                            let _ = writeln!(out, "get_block failed: {e}");
                        }
                    }
                    continue;
                }

                let input = make_turn_input(&line);
                match session.step_with_agent_loop(input).await {
                    Ok(reply) => {
                        // Agent output was already streamed via CliDisplaySubscriber.
                        // Print a one-line cache summary from the last wire turn's metrics.
                        let last = reply.turns.last().expect("at least one wire turn");
                        let m = &last.cache_metrics;
                        let Ok(mut out) = writer.lock() else {
                            continue;
                        };
                        use std::io::Write as _;
                        let _ = writeln!(
                            out,
                            "[cache: fresh={} read={} create={} ratio={:.0}%]",
                            m.fresh_input_tokens,
                            m.cache_read_input_tokens,
                            m.cache_creation_input_tokens,
                            m.hit_ratio() * 100.0,
                        );
                    }
                    Err(e) => {
                        let Ok(mut out) = writer.lock() else {
                            continue;
                        };
                        use std::io::Write as _;
                        let _ = writeln!(out, "error: {e}");
                    }
                }
            }
            Ok(rustyline_async::ReadlineEvent::Eof) => break,
            Ok(rustyline_async::ReadlineEvent::Interrupted) => break,
            Err(ReadlineError::Closed) => break,
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }

    eprintln!("[spawn] session ended.");
    Ok(())
}

fn print_turn_metrics(
    label: &str,
    reply: &pattern_core::types::turn::StepReply,
    duration: std::time::Duration,
) {
    let last = reply.turns.last().expect("at least one wire turn");
    let m = &last.cache_metrics;
    let hit_ratio = m.hit_ratio();
    let usage = last.usage.as_ref();
    let prompt = usage.and_then(|u| u.prompt_tokens).unwrap_or(0);
    let completion = usage.and_then(|u| u.completion_tokens).unwrap_or(0);
    let total = usage.and_then(|u| u.total_tokens).unwrap_or(0);
    eprintln!(
        "  [{label}]\n\
         \x20   wire_turns={} stop={:?} duration={:.2}s\n\
         \x20   usage: prompt={prompt} completion={completion} total={total}\n\
         \x20   cache: fresh={} read={} create={} (hit_ratio={:.3})",
        reply.turns.len(),
        reply.final_stop_reason,
        duration.as_secs_f64(),
        m.fresh_input_tokens,
        m.cache_read_input_tokens,
        m.cache_creation_input_tokens,
        hit_ratio,
    );
}
