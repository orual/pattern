//! Pattern Discord plugin — v0.1.
//!
//! Exposes a `discord` Port: subscribe for inbound message events,
//! call `send_message` for outbound. Serenity-backed gateway loop
//! runs in a tokio task; events fan out via a broadcast channel that
//! per-subscribe streams filter by channel-id.
//!
//! Out of scope for v0.1 (queued):
//!  - batching + gap-context preprocessing
//!  - slash command relay via TUI channel
//!  - reactions, edits, threads, attachments
//!  - mention rewriting + author display-name lookup

use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt as _;
use pattern_plugin_sdk::{PluginContext, PluginError, PluginExtension, Port};
use pattern_plugin_sdk::{PortCapabilities, PortError, PortEvent, PortId, PortMetadata};
use serde::Deserialize;
use serenity::all::{ChannelId, GatewayIntents, Http, Message, Ready};
use serenity::client::{Client, Context, EventHandler};
use tokio::sync::{broadcast, OnceCell};

/// Capacity of the in-process fan-out broadcast channel.
const EVENT_BROADCAST_CAPACITY: usize = 1024;

/// The `discord` Port. Holds a serenity Http client for outbound calls
/// and a broadcast Sender that the gateway EventHandler writes inbound
/// events into. Per-`subscribe` streams construct a fresh Receiver and
/// filter to the channels listed in the subscribe config.
#[derive(Debug)]
struct DiscordPort {
    id: PortId,
    http: OnceCell<Arc<Http>>,
    events_tx: broadcast::Sender<PortEvent>,
}

impl DiscordPort {
    fn new() -> Self {
        let (events_tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        Self {
            id: PortId::new("discord"),
            http: OnceCell::new(),
            events_tx,
        }
    }

    fn events_tx(&self) -> broadcast::Sender<PortEvent> { self.events_tx.clone() }

    async fn set_http(&self, http: Arc<Http>) {
        let _ = self.http.set(http);
    }

    fn http(&self) -> Result<Arc<Http>, PortError> {
        self.http.get().cloned().ok_or_else(|| PortError::Internal {
            port: self.id.clone(),
            message: "discord client not yet initialized (gateway still starting up)".into(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct SubscribeConfig {
    /// Discord channel IDs (snowflake strings). Empty = subscribe to ALL
    /// channels the bot can see (use sparingly).
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
        PortMetadata::new(self.id.clone(), "Discord chat: subscribe to channel messages + send replies")
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_methods(["send_message"])
    }

    fn capabilities(&self) -> PortCapabilities {
        PortCapabilities::default().with_callable(true).with_subscribable(true)
    }

    async fn subscribe(
        &self,
        config: serde_json::Value,
    ) -> Result<BoxStream<'static, PortEvent>, PortError> {
        let cfg: SubscribeConfig = serde_json::from_value(config).map_err(|e| PortError::InvalidPayload {
            port: self.id.clone(),
            method: "subscribe".into(),
            message: format!("subscribe config: {e}").into(),
        })?;
        let allow: std::collections::HashSet<String> = cfg.channels.into_iter().collect();
        let rx = self.events_tx.subscribe();
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(move |item| {
                let allow = allow.clone();
                async move {
                    let ev = item.ok()?;
                    if allow.is_empty() { return Some(ev); }
                    let ch = ev.payload.get("channel_id").and_then(|v| v.as_str())?;
                    if allow.contains(ch) { Some(ev) } else { None }
                }
            });
        Ok(Box::pin(stream))
    }

    async fn call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, PortError> {
        match method {
            "send_message" => {
                let req: SendMessageReq = serde_json::from_value(payload).map_err(|e| PortError::InvalidPayload {
                    port: self.id.clone(),
                    method: method.into(),
                    message: e.to_string().into(),
                })?;
                let http = self.http()?;
                let ch_id: u64 = req.channel_id.parse().map_err(|e| PortError::InvalidPayload {
                    port: self.id.clone(),
                    method: method.into(),
                    message: format!("channel_id parse: {e}").into(),
                })?;
                let msg = ChannelId::new(ch_id)
                    .say(http.as_ref(), &req.content)
                    .await
                    .map_err(|e| PortError::Internal {
                        port: self.id.clone(),
                        message: format!("discord send: {e}").into(),
                    })?;
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

/// serenity EventHandler that converts gateway events into PortEvents on
/// the plugin's broadcast channel.
struct DiscordHandler {
    events_tx: broadcast::Sender<PortEvent>,
    port: Arc<DiscordPort>,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!(bot = %ready.user.name, guilds = ready.guilds.len(), "discord gateway ready");
        // Stash the Http for outbound calls now that we have a live ctx.
        self.port.set_http(ctx.http.clone()).await;
    }

    async fn message(&self, _ctx: Context, msg: Message) {
        // Always exclude bot's own messages.
        if msg.author.bot { return; }
        let payload = serde_json::json!({
            "kind": "message",
            "channel_id": msg.channel_id.to_string(),
            "author_id": msg.author.id.to_string(),
            "author_name": msg.author.name,
            "content": msg.content,
            "message_id": msg.id.to_string(),
            "is_dm": msg.guild_id.is_none(),
            "guild_id": msg.guild_id.map(|g| g.to_string()),
            "reply_to_message_id": msg.referenced_message.as_ref().map(|m| m.id.to_string()),
        });
        let ev = PortEvent::new(PortId::new("discord"), payload, jiff::Timestamp::now());
        let _ = self.events_tx.send(ev); // drop on no-receivers
    }
}

#[derive(Debug)]
struct DiscordPlugin {
    port: Arc<DiscordPort>,
}

impl DiscordPlugin {
    fn new() -> Self { Self { port: Arc::new(DiscordPort::new()) } }
}

#[async_trait]
impl PluginExtension for DiscordPlugin {
    fn ports(&self) -> Vec<Arc<dyn Port>> { vec![self.port.clone() as Arc<dyn Port>] }

    async fn on_enable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        // Bot token from .env. Plugin's working dir is the cache subdir;
        // dotenvy in main.rs loaded it before we got here.
        let token = std::env::var("PATTERN_DISCORD_BOT_TOKEN")
            .map_err(|_| PluginError::ConfigError("PATTERN_DISCORD_BOT_TOKEN not set in plugin .env".into()))?;

        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;

        let handler = DiscordHandler {
            events_tx: self.port.events_tx(),
            port: self.port.clone(),
        };

        let mut client = Client::builder(&token, intents)
            .event_handler(handler)
            .await
            .map_err(|e| PluginError::ConfigError(format!("discord client build: {e}").into()))?;

        // Run the gateway loop in a detached task; on_enable returns once
        // the loop is spawned. Errors inside the loop log + the task exits.
        tokio::spawn(async move {
            if let Err(e) = client.start().await {
                tracing::error!(error = %e, "discord gateway loop exited");
            }
        });

        Ok(())
    }

    async fn on_disable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        tracing::info!("discord plugin on_disable (gateway task will be cancelled on process exit)");
        Ok(())
    }
}

// Helper to satisfy anyhow::Context usage when constructing PluginError
// — keeps the dependency list slim by not requiring miette.
fn _anyhow_link() -> anyhow::Result<()> { Ok(()).context("link") }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let _ = dotenvy::dotenv();

    let plugin = DiscordPlugin::new();
    let _handle = pattern_plugin_sdk::register_plugin("pattern-discord".into(), plugin).await?;

    tokio::signal::ctrl_c().await?;
    Ok(())
}
