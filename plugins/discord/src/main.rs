//! Pattern Discord plugin — v0.1.
//!
//! Architecture (per discord plugin v0.1 design lock):
//!  - DiscordPort with subscribe / unsubscribe / call("send_message").
//!  - subscribe/unsubscribe is an on/off switch for inbound TUI-channel
//!    forwarding, NOT a stream of events through the port itself.
//!  - Inbound discord messages on subscribed channels mint a batch_id,
//!    dial the daemon TUI channel, submit as a user-message with
//!    Author/Sphere derived from discord context.
//!  - Plugin subscribes to TaggedTurnEvent stream (subscribe_all), filters
//!    to its own minted batch_ids, accumulates WireTurnEvents, posts to
//!    the originating discord channel on Stop(EndTurn).
//!
//! Out of scope for v0.1 (queued):
//!  - message-batching (5/10s window), gap-context, channel-name resolution
//!  - mention rewrite either direction
//!  - slash command relay
//!  - reactions/edits/readMessages/listThreads
//!  - per-sphere formatter (v0.1 ships text-only; v0.2 will surface thinking/tool-calls in Private)

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use miette::{IntoDiagnostic, Result};
use pattern_plugin_sdk::tui_channel::{
    new_snowflake_id, Author, BatchId, ContentPart, DaemonClient, Human, MessageOrigin,
    Partner, Recipient, Sphere, TaggedTurnEvent, WireTurnEvent,
};
use pattern_plugin_sdk::{
    PluginContext, PluginError, PluginExtension, Port, PortCapabilities, PortError,
    PortEvent, PortId, PortMetadata,
};
use serde::Deserialize;
use serenity::all::{ChannelId, GatewayIntents, Http, Message, Ready};
use serenity::client::{Client, Context, EventHandler};
use smol_str::SmolStr;
use tokio::sync::{Mutex, OnceCell};

/// Per-batch accumulator. Holds the raw events so v0.2 can format them
/// per-sphere (thinking + tool-calls in Private, text in Public, etc).
/// v0.1 formatter only extracts WireTurnEvent::Text.
#[derive(Debug)]
struct BatchState {
    channel_id: ChannelId,
    sphere: Sphere,
    /// Raw event log for the batch; v0.2 formatter will use this for
    /// per-sphere rendering of thinking/tool-calls/etc.
    events: Vec<WireTurnEvent>,
    /// Pending text-only buffer for incremental discord posts. Flushed on
    /// newline-in-content OR on any Stop(...). Cleared after each flush.
    pending_text: String,
}

/// Shared plugin state. EventHandler, DiscordPort, and the subscribe_all
/// drainer task all hold an Arc<DiscordState>.
#[derive(Debug, Default)]
struct DiscordState {
    /// Channel IDs the plugin is currently forwarding to the TUI channel.
    /// Populated by Port.subscribe; cleared by Port.unsubscribe.
    active_channels: Mutex<HashSet<u64>>,
    /// Outstanding batches minted by this plugin instance.
    pending_batches: Mutex<HashMap<BatchId, BatchState>>,
    /// Set once the serenity gateway hits `ready`; outbound call() reads from here.
    http: OnceCell<Arc<Http>>,
    /// Discord user IDs to treat as Author::Partner. Read from
    /// PATTERN_DISCORD_PARTNER_IDS in on_enable (deferred so install-time
    /// `--pattern-plugin-init` doesn't require runtime env).
    partner_ids: Mutex<HashSet<u64>>,
}

impl DiscordState {
    async fn author_for(&self, user_id: u64, display_name: String) -> Author {
        if self.partner_ids.lock().await.contains(&user_id) {
            Author::Partner(Partner {
                user_id: user_id.to_string().into(),
                display_name: Some(display_name),
            })
        } else {
            Author::Human(Human {
                user_id: user_id.to_string().into(),
                display_name: Some(display_name),
            })
        }
    }
}

/// Sphere derivation for v0.1. Conservative default: any non-DM is SemiPrivate.
/// Public broadcast (open guild, megaphone channels) is not yet distinguished —
/// v0.2 can split via guild metadata or per-channel override.
fn sphere_for(is_dm: bool) -> Sphere {
    if is_dm { Sphere::Private } else { Sphere::SemiPrivate }
}

// ── DiscordPort ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct DiscordPort {
    id: PortId,
    state: Arc<DiscordState>,
}

#[derive(Debug, Deserialize)]
struct SubscribeConfig {
    /// Discord channel IDs (snowflake strings).
    #[serde(default)]
    channels: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SendMessageReq {
    channel_id: String,
    content: String,
}

#[async_trait]
impl Port for DiscordPort {
    fn id(&self) -> &PortId { &self.id }

    fn metadata(&self) -> PortMetadata {
        PortMetadata::new(self.id.clone(), "Discord: subscribe channels for auto-reply; call send_message for outbound")
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_methods(["send_message"])
    }

    fn capabilities(&self) -> PortCapabilities {
        PortCapabilities::default().with_callable(true).with_subscribable(true)
    }

    async fn subscribe(&self, config: serde_json::Value) -> Result<BoxStream<'static, PortEvent>, PortError> {
        tracing::info!(?config, "DiscordPort.subscribe called");
        let cfg: SubscribeConfig = serde_json::from_value(config).map_err(|e| PortError::CallFailed(
            self.id.clone(),
            format!("subscribe config: {e}"),
        ))?;
        let mut active = self.state.active_channels.lock().await;
        for ch in &cfg.channels {
            if let Ok(n) = ch.parse::<u64>() { active.insert(n); }
        }
        tracing::info!(channels = ?cfg.channels, active_count = active.len(), "subscribed channels");
        // Events don't flow through the port — they go via the TUI channel.
        // Returning an empty stream satisfies the trait while keeping the
        // subscribe semantics as a toggle.
        Ok(Box::pin(futures::stream::empty()))
    }

    async fn unsubscribe(&self) -> Result<(), PortError> {
        self.state.active_channels.lock().await.clear();
        Ok(())
    }

    async fn call(&self, method: &str, payload: serde_json::Value) -> Result<serde_json::Value, PortError> {
        match method {
            "send_message" => {
                let req: SendMessageReq = serde_json::from_value(payload).map_err(|e| {
                    PortError::CallFailed(self.id.clone(), format!("send_message payload: {e}"))
                })?;
                let http = self.state.http.get().cloned().ok_or_else(|| {
                    PortError::CallFailed(self.id.clone(), "discord client not yet ready".into())
                })?;
                let ch_id: u64 = req.channel_id.parse().map_err(|e| {
                    PortError::CallFailed(self.id.clone(), format!("channel_id parse: {e}"))
                })?;
                let msg = ChannelId::new(ch_id)
                    .say(http.as_ref(), &req.content)
                    .await
                    .map_err(|e| PortError::CallFailed(self.id.clone(), format!("discord send: {e}")))?;
                Ok(serde_json::json!({ "message_id": msg.id.to_string() }))
            }
            other => Err(PortError::UnsupportedMethod {
                port: self.id.clone(),
                method: other.into(),
            }),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any { self }
}

// ── Serenity event handler: discord → TUI-channel user-message ───────────────

struct DiscordHandler {
    state: Arc<DiscordState>,
    client: Arc<DaemonClient>,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!(bot = %ready.user.name, guilds = ready.guilds.len(), "discord gateway ready");
        let _ = self.state.http.set(ctx.http.clone());
    }

    async fn message(&self, _ctx: Context, msg: Message) {
        tracing::info!(
            author = %msg.author.name,
            is_bot = msg.author.bot,
            channel_id = %msg.channel_id,
            is_dm = msg.guild_id.is_none(),
            content_len = msg.content.len(),
            content = %msg.content,
            "discord message received",
        );
        if msg.author.bot { return; }
        let ch_u64: u64 = msg.channel_id.into();
        let active = self.state.active_channels.lock().await;
        let in_active = active.contains(&ch_u64);
        tracing::info!(channel_id = ch_u64, in_active, active_size = active.len(), "active-channel check");
        if !in_active { return; }
        drop(active);

        let is_dm = msg.guild_id.is_none();
        let user_id: u64 = msg.author.id.into();
        let display = msg.author.global_name.clone().unwrap_or_else(|| msg.author.name.clone());
        let author = self.state.author_for(user_id, display.clone()).await;
        let sphere = sphere_for(is_dm);
        // transport_hint exposes the user-facing surface label so the
        // composer can render attribution as `via discord:DM:orual` etc.
        // v0.1: DM hint includes partner display-name; guild uses channel_id
        // (channel-name resolution is v0.2).
        let hint: smol_str::SmolStr = if is_dm {
            smol_str::SmolStr::from(format!("discord:DM:{}", display))
        } else {
            let ch_u64: u64 = msg.channel_id.into();
            smol_str::SmolStr::from(format!("discord:channel:{}", ch_u64))
        };
        let origin = MessageOrigin::new(author, sphere).with_transport_hint(hint);

        let batch_id: BatchId = new_snowflake_id();
        self.state.pending_batches.lock().await.insert(
            batch_id.clone(),
            BatchState { channel_id: msg.channel_id, sphere, events: Vec::new(), pending_text: String::new() },
        );

        let parts = vec![ContentPart::Text(msg.content.clone())];
        tracing::info!(
            %batch_id,
            channel_id = ch_u64,
            parts_count = parts.len(),
            first_text_len = msg.content.len(),
            "submitting user-message to daemon",
        );
        if let Err(e) = self.client
            .send_message(batch_id.clone(), Recipient::Auto, parts, origin)
            .await
        {
            tracing::warn!(error = ?e, %batch_id, "discord → daemon send_message failed");
            self.state.pending_batches.lock().await.remove(&batch_id);
        } else {
            tracing::info!(%batch_id, "daemon accepted submitted batch");
        }
    }
}

// ── Subscribe-all drainer: TUI channel → discord channel ─────────────────────

/// Drains TaggedTurnEvent stream; on Stop(EndTurn) for an owned batch_id,
/// formats the accumulated events for the batch's sphere and posts to discord.
async fn drain_turn_events(
    state: Arc<DiscordState>,
    mut rx: irpc::channel::mpsc::Receiver<TaggedTurnEvent>,
) {
    use pattern_core::types::turn::StopReason;
    while let Ok(Some(tagged)) = rx.recv().await {
        let batch_id = tagged.batch_id.clone();

        // Decide flush + drop semantics for this event.
        // - any Stop(*): flush pending_text (so partial output before a
        //   tool-call lands in the channel).
        // - Stop(EndTurn) specifically: also drop the batch from pending.
        // - Text(s) containing '\n': flush at the last newline boundary;
        //   keep the tail-after-newline buffered for the next chunk.
        let is_stop = matches!(&tagged.event, WireTurnEvent::Stop(_));
        let is_endturn = matches!(
            &tagged.event,
            WireTurnEvent::Stop(StopReason::EndTurn)
        );

        let mut pending = state.pending_batches.lock().await;
        let owned = pending.contains_key(&batch_id);
        tracing::debug!(%batch_id, owned, "drainer received event: {:?}", tagged.event);
        let Some(entry) = pending.get_mut(&batch_id) else { continue };

        entry.events.push(tagged.event.clone());

        // Build flush candidate string + remaining buffer.
        let mut flush_now: Option<String> = None;
        if let WireTurnEvent::Text(s) = &tagged.event {
            entry.pending_text.push_str(s);
            if let Some(last_nl) = entry.pending_text.rfind('\n') {
                let (head, tail) = entry.pending_text.split_at(last_nl + 1);
                flush_now = Some(head.to_string());
                entry.pending_text = tail.to_string();
            }
        }
        if is_stop && !entry.pending_text.is_empty() {
            flush_now = Some(std::mem::take(&mut entry.pending_text));
        }

        let channel_id = entry.channel_id;
        drop(pending);

        // On Stop(EndTurn), spawn a delayed cleanup rather than removing
        // immediately. Late Text/Stop events on the same batch — fanout
        // timing, network re-delivery, etc — should still match + flush
        // during the grace window. After the window, remove from pending
        // so the map doesn't grow unbounded.
        if is_endturn {
            let state2 = Arc::clone(&state);
            let bid = batch_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                state2.pending_batches.lock().await.remove(&bid);
            });
        }

        let Some(to_post) = flush_now else { continue };
        if to_post.is_empty() { continue }
        let Some(http) = state.http.get().cloned() else {
            tracing::warn!(%batch_id, "discord http not ready; dropping reply");
            continue;
        };
        // Discord caps single-message content at 2000 chars. Greedy-pack
        // lines under that limit; preserves newline formatting within each post.
        for chunk in chunk_for_discord(&to_post, 1900) {
            if let Err(e) = channel_id.say(http.as_ref(), &chunk).await {
                tracing::warn!(%batch_id, %channel_id, error = %e, "discord reply post failed");
            }
        }
    }
}

/// v0.1 formatter: text-only, regardless of sphere.
/// v0.2 will branch: Private gets thinking + tool-calls + text, SemiPrivate
/// gets text + light tool-trace, Public stays text-only.
fn format_for_sphere(_sphere: Sphere, events: &[WireTurnEvent]) -> String {
    let mut out = String::new();
    for ev in events {
        if let WireTurnEvent::Text(s) = ev {
            out.push_str(s);
        }
    }
    out
}

/// Split text into discord-sendable chunks. Greedy-packs lines (preserving
/// `\n` separators within each chunk) up to `max` chars per post. Lines that
/// individually exceed `max` get hard-split at char boundary.
///
/// Code-block-aware: tracks ``` fence state and defers chunk boundaries while
/// inside an open fence. If a fence-content run exceeds `max`, falls back to
/// closing the fence at chunk-end and reopening at next-chunk-start with the
/// same language tag (so each chunk renders as a code block on Discord).
fn chunk_for_discord(text: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_fence = false;
    let mut fence_lang: String = String::new(); // language tag of the open fence

    let lines: Vec<&str> = text.split('\n').collect();
    for line in lines {
        let is_fence = line.trim_start().starts_with("```");
        let fence_lang_this = if is_fence {
            line.trim_start().trim_start_matches("```").trim().to_string()
        } else {
            String::new()
        };

        let line_len = line.chars().count();
        // Hard-cap on a single line outside a fence — fall through to greedy.
        // Inside a fence we'll close+reopen across chunks to preserve rendering.
        let separator_len = if cur.is_empty() { 0 } else { 1 };
        let close_reopen_overhead = if in_fence { 4 + fence_lang.chars().count() + 1 + 4 } else { 0 };
        let needed = cur.chars().count() + separator_len + line_len;
        let would_exceed = needed + close_reopen_overhead > max;

        if would_exceed && !cur.is_empty() {
            // Flush current chunk. If we're mid-fence, close it before flushing
            // and reopen at the start of the next chunk.
            if in_fence {
                cur.push('\n');
                cur.push_str("```");
            }
            out.push(std::mem::take(&mut cur));
            if in_fence {
                cur.push_str("```");
                cur.push_str(&fence_lang);
            }
        }

        // Append the line.
        if !cur.is_empty() { cur.push('\n'); }
        cur.push_str(line);

        // Update fence state AFTER appending so the toggling line itself
        // lands inside the same chunk it was emitted from.
        if is_fence {
            if in_fence {
                in_fence = false;
                fence_lang.clear();
            } else {
                in_fence = true;
                fence_lang = fence_lang_this;
            }
        }

        // If a single line outside a fence exceeds max, hard-split it now.
        if !in_fence && line_len > max {
            // Pull the just-pushed line back out + hard-split.
            // Simpler: trust the greedy pack already flushed prior; if cur
            // is still over max we hard-split as a final pass below.
        }
    }
    if !cur.is_empty() { out.push(cur); }

    // Final pass: any chunk still over max gets hard-split by char.
    let mut final_out: Vec<String> = Vec::new();
    for chunk in out {
        if chunk.chars().count() <= max {
            final_out.push(chunk);
        } else {
            let mut buf = String::new();
            for ch in chunk.chars() {
                if buf.chars().count() >= max {
                    final_out.push(std::mem::take(&mut buf));
                }
                buf.push(ch);
            }
            if !buf.is_empty() { final_out.push(buf); }
        }
    }
    final_out
}

// ── PluginExtension ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct DiscordPlugin {
    port: Arc<DiscordPort>,
    state: Arc<DiscordState>,
}

#[async_trait]
impl PluginExtension for DiscordPlugin {
    fn ports(&self) -> Vec<Arc<dyn Port>> { vec![self.port.clone() as Arc<dyn Port>] }

    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let mount = ctx.mount_path.clone().ok_or_else(|| {
            PluginError::Lifecycle("discord plugin requires mount_path in PluginContext".into())
        })?;

        // Read runtime env. dotenv pulls in the plugin cache dir's .env where
        // PATTERN_DISCORD_BOT_TOKEN + PATTERN_DISCORD_PARTNER_IDS live.
        let _ = dotenvy::dotenv();
        let bot_token = std::env::var("PATTERN_DISCORD_BOT_TOKEN")
            .map_err(|_| PluginError::Lifecycle(
                "PATTERN_DISCORD_BOT_TOKEN not set in plugin .env".into(),
            ))?;
        let partner_ids: HashSet<u64> = std::env::var("PATTERN_DISCORD_PARTNER_IDS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse::<u64>().ok())
            .collect();
        // Stash partner_ids on the shared state for the EventHandler.
        *self.state.partner_ids.lock().await = partner_ids;

        // Connect to the daemon TUI channel.
        let client = Arc::new(
            DaemonClient::connect()
                .await
                .map_err(|e| PluginError::Lifecycle(format!("daemon connect: {e}")))?,
        );

        // Subscribe to mount-wide turn events; spawn drainer.
        let rx = client
            .subscribe_all(mount.clone())
            .await
            .map_err(|e| PluginError::Lifecycle(format!("subscribe_all: {e}")))?;
        let drainer_state = self.state.clone();
        tokio::spawn(drain_turn_events(drainer_state, rx));

        // Spawn the serenity gateway.
        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;
        let handler = DiscordHandler { state: self.state.clone(), client: client.clone() };
        let mut serenity_client = Client::builder(&bot_token, intents)
            .event_handler(handler)
            .await
            .map_err(|e| PluginError::Lifecycle(format!("serenity client build: {e}")))?;
        tokio::spawn(async move {
            if let Err(e) = serenity_client.start().await {
                tracing::error!(error = %e, "discord gateway loop exited");
            }
        });

        Ok(())
    }

    async fn on_disable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        tracing::info!("discord plugin on_disable (gateway task will cancel at process exit)");
        Ok(())
    }
}

// ── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Default to info level when RUST_LOG isn't set — `from_default_env()`
    // returns an empty filter when the env is unset, which suppresses ALL
    // output including panics. Fall back to info+.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().json().with_env_filter(filter).init();

    // Env (token, partner-allowlist) is read in on_enable so install-time
    // `--pattern-plugin-init` mode — which register_plugin short-circuits
    // before any plugin code runs — doesn't require runtime env.
    let state = Arc::new(DiscordState::default());
    let port = Arc::new(DiscordPort {
        id: PortId::new("discord"),
        state: state.clone(),
    });
    let plugin = DiscordPlugin { port, state };

    let _handle = pattern_plugin_sdk::register_plugin("pattern-discord".into(), plugin)
        .await
        .into_diagnostic()?;

    // Wait for either SIGINT (ctrl_c) or SIGTERM (sent by daemon at
    // /shutdown). Both should cause clean exit. SIGTERM is the load-bearing
    // case — daemon's PluginRegistry::shutdown_all() sends SIGTERM to OOP
    // children before std::process::exit, so plugins need to respect it.
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ).into_diagnostic()?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM");
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.into_diagnostic()?;
    }
    Ok(())
}
