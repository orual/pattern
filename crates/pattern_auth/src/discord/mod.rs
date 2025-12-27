//! Discord authentication and configuration module.
//!
//! This module provides storage for Discord bot configuration,
//! enabling Pattern agents to maintain Discord integration settings across restarts.
//!
//! Configuration can be loaded from environment variables via `DiscordBotConfig::from_env()`
//! or retrieved from the database via `AuthDb::get_discord_bot_config()`.

mod bot_config;

pub use bot_config::DiscordBotConfig;
