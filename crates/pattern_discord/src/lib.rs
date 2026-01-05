//! Pattern Discord - Discord Bot Integration
//!
//! This crate provides Discord bot functionality for Pattern,
//! enabling natural language interaction with the multi-agent system.
//!
//! ## Configuration
//!
//! The bot uses `pattern_auth::DiscordBotConfig` for configuration.
//! Configuration can be loaded from:
//! - Environment variables via `DiscordBotConfig::from_env()`
//! - Database via `AuthDb::get_discord_bot_config()`
//!
//! The config should be loaded once at startup and passed to the bot.
//! There are NO runtime environment variable reads in this crate.

pub mod bot;
pub mod commands;
pub mod context;
//pub mod data_source;
pub mod endpoints;
pub mod error;
pub mod helpers;
pub mod routing;
pub mod slash_commands;

pub use bot::{DiscordBot, DiscordBotConfig, DiscordEventHandler};
pub use commands::{Command, CommandHandler, SlashCommand};
pub use context::{DiscordContext, MessageContext, UserContext};
pub use error::{DiscordError, Result};
pub use routing::{MessageRouter, RoutingStrategy};

// Re-export serenity for convenience
pub use serenity;

// Re-export pattern_auth for config access
pub use pattern_auth;

/// Re-export commonly used types
pub mod prelude {
    pub use crate::{
        Command, CommandHandler, DiscordBot, DiscordBotConfig, DiscordContext, DiscordError,
        MessageContext, MessageRouter, Result, RoutingStrategy, SlashCommand, UserContext,
    };
}

#[cfg(test)]
mod tests {

    #[test]
    fn it_works() {
        // Basic smoke test
        assert_eq!(2 + 2, 4);
    }
}
