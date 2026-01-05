//! Discord bot configuration storage.
//!
//! This module provides `DiscordBotConfig` for storing Discord bot credentials
//! and access control settings.

use crate::db::AuthDb;
use crate::error::AuthResult;

/// Discord bot configuration.
///
/// Stores bot credentials and access control settings for a Pattern constellation.
/// This is a singleton configuration (only one per auth database).
#[derive(Debug, Clone)]
pub struct DiscordBotConfig {
    /// Discord bot token (required).
    pub bot_token: String,
    /// Discord application ID.
    pub app_id: Option<String>,
    /// Discord public key for webhook verification.
    pub public_key: Option<String>,
    /// List of allowed channel IDs.
    pub allowed_channels: Option<Vec<String>>,
    /// List of allowed guild IDs.
    pub allowed_guilds: Option<Vec<String>>,
    /// List of admin user IDs.
    pub admin_users: Option<Vec<String>>,
    /// Default user ID for DMs.
    pub default_dm_user: Option<String>,
}

impl DiscordBotConfig {
    /// Load Discord bot configuration from environment variables.
    ///
    /// Returns `None` if `DISCORD_TOKEN` is not set.
    ///
    /// # Environment Variables
    ///
    /// - `DISCORD_TOKEN` -> bot_token (required for Some result)
    /// - `APP_ID` or `DISCORD_CLIENT_ID` -> app_id
    /// - `DISCORD_PUBLIC_KEY` -> public_key
    /// - `DISCORD_CHANNEL_ID` (comma-separated) -> allowed_channels
    /// - `DISCORD_GUILD_IDS` or `DISCORD_GUILD_ID` (comma-separated) -> allowed_guilds
    /// - `DISCORD_ADMIN_USERS` or `DISCORD_DEFAULT_DM_USER` (comma-separated) -> admin_users
    /// - `DISCORD_DEFAULT_DM_USER` -> default_dm_user
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("DISCORD_TOKEN").ok()?;

        let app_id = std::env::var("APP_ID")
            .ok()
            .or_else(|| std::env::var("DISCORD_CLIENT_ID").ok());

        let public_key = std::env::var("DISCORD_PUBLIC_KEY").ok();

        let allowed_channels = std::env::var("DISCORD_CHANNEL_ID")
            .ok()
            .map(|s| parse_comma_separated(&s));

        let allowed_guilds = std::env::var("DISCORD_GUILD_IDS")
            .ok()
            .or_else(|| std::env::var("DISCORD_GUILD_ID").ok())
            .map(|s| parse_comma_separated(&s));

        let admin_users = std::env::var("DISCORD_ADMIN_USERS")
            .ok()
            .or_else(|| std::env::var("DISCORD_DEFAULT_DM_USER").ok())
            .map(|s| parse_comma_separated(&s));

        let default_dm_user = std::env::var("DISCORD_DEFAULT_DM_USER").ok();

        Some(Self {
            bot_token,
            app_id,
            public_key,
            allowed_channels,
            allowed_guilds,
            admin_users,
            default_dm_user,
        })
    }
}

/// Parse a comma-separated string into a Vec of trimmed, non-empty strings.
fn parse_comma_separated(s: &str) -> Vec<String> {
    s.split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Database row for discord_bot_config table.
#[derive(Debug, sqlx::FromRow)]
struct DiscordBotConfigRow {
    bot_token: String,
    app_id: Option<String>,
    public_key: Option<String>,
    allowed_channels: Option<String>,
    allowed_guilds: Option<String>,
    admin_users: Option<String>,
    default_dm_user: Option<String>,
}

impl DiscordBotConfigRow {
    /// Convert database row to DiscordBotConfig.
    fn to_config(&self) -> AuthResult<DiscordBotConfig> {
        let allowed_channels = self
            .allowed_channels
            .as_ref()
            .map(|s| serde_json::from_str(s))
            .transpose()?;

        let allowed_guilds = self
            .allowed_guilds
            .as_ref()
            .map(|s| serde_json::from_str(s))
            .transpose()?;

        let admin_users = self
            .admin_users
            .as_ref()
            .map(|s| serde_json::from_str(s))
            .transpose()?;

        Ok(DiscordBotConfig {
            bot_token: self.bot_token.clone(),
            app_id: self.app_id.clone(),
            public_key: self.public_key.clone(),
            allowed_channels,
            allowed_guilds,
            admin_users,
            default_dm_user: self.default_dm_user.clone(),
        })
    }
}

impl AuthDb {
    /// Get the Discord bot configuration from the database.
    ///
    /// Returns `None` if no configuration has been stored.
    pub async fn get_discord_bot_config(&self) -> AuthResult<Option<DiscordBotConfig>> {
        let row = sqlx::query_as!(
            DiscordBotConfigRow,
            r#"
            SELECT
                bot_token as "bot_token!",
                app_id,
                public_key,
                allowed_channels,
                allowed_guilds,
                admin_users,
                default_dm_user
            FROM discord_bot_config
            WHERE id = 1
            "#
        )
        .fetch_optional(self.pool())
        .await?;

        match row {
            Some(row) => Ok(Some(row.to_config()?)),
            None => Ok(None),
        }
    }

    /// Store Discord bot configuration in the database.
    ///
    /// This performs an upsert, creating or updating the singleton configuration.
    pub async fn set_discord_bot_config(&self, config: &DiscordBotConfig) -> AuthResult<()> {
        let allowed_channels_json = config
            .allowed_channels
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        let allowed_guilds_json = config
            .allowed_guilds
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        let admin_users_json = config
            .admin_users
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        let now = chrono::Utc::now().timestamp();

        sqlx::query!(
            r#"
            INSERT INTO discord_bot_config (
                id, bot_token, app_id, public_key,
                allowed_channels, allowed_guilds, admin_users, default_dm_user,
                created_at, updated_at
            ) VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (id) DO UPDATE SET
                bot_token = excluded.bot_token,
                app_id = excluded.app_id,
                public_key = excluded.public_key,
                allowed_channels = excluded.allowed_channels,
                allowed_guilds = excluded.allowed_guilds,
                admin_users = excluded.admin_users,
                default_dm_user = excluded.default_dm_user,
                updated_at = excluded.updated_at
            "#,
            config.bot_token,
            config.app_id,
            config.public_key,
            allowed_channels_json,
            allowed_guilds_json,
            admin_users_json,
            config.default_dm_user,
            now,
            now,
        )
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Delete the Discord bot configuration from the database.
    pub async fn delete_discord_bot_config(&self) -> AuthResult<()> {
        sqlx::query!("DELETE FROM discord_bot_config WHERE id = 1")
            .execute(self.pool())
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_discord_config_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Initially no config
        let config = db.get_discord_bot_config().await.unwrap();
        assert!(config.is_none());

        // Create and store config
        let config = DiscordBotConfig {
            bot_token: "test-bot-token".to_string(),
            app_id: Some("123456789".to_string()),
            public_key: Some("public-key-hex".to_string()),
            allowed_channels: Some(vec!["channel1".to_string(), "channel2".to_string()]),
            allowed_guilds: Some(vec!["guild1".to_string()]),
            admin_users: Some(vec!["admin1".to_string(), "admin2".to_string()]),
            default_dm_user: Some("dm-user".to_string()),
        };

        db.set_discord_bot_config(&config).await.unwrap();

        // Retrieve and verify
        let retrieved = db
            .get_discord_bot_config()
            .await
            .unwrap()
            .expect("config should exist");

        assert_eq!(retrieved.bot_token, "test-bot-token");
        assert_eq!(retrieved.app_id, Some("123456789".to_string()));
        assert_eq!(retrieved.public_key, Some("public-key-hex".to_string()));
        assert_eq!(
            retrieved.allowed_channels,
            Some(vec!["channel1".to_string(), "channel2".to_string()])
        );
        assert_eq!(retrieved.allowed_guilds, Some(vec!["guild1".to_string()]));
        assert_eq!(
            retrieved.admin_users,
            Some(vec!["admin1".to_string(), "admin2".to_string()])
        );
        assert_eq!(retrieved.default_dm_user, Some("dm-user".to_string()));

        // Delete and verify
        db.delete_discord_bot_config().await.unwrap();
        let deleted = db.get_discord_bot_config().await.unwrap();
        assert!(deleted.is_none());
    }

    #[tokio::test]
    async fn test_discord_config_update() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Create initial config
        let config = DiscordBotConfig {
            bot_token: "token-1".to_string(),
            app_id: None,
            public_key: None,
            allowed_channels: None,
            allowed_guilds: None,
            admin_users: None,
            default_dm_user: None,
        };

        db.set_discord_bot_config(&config).await.unwrap();

        // Update config
        let updated_config = DiscordBotConfig {
            bot_token: "token-2".to_string(),
            app_id: Some("new-app-id".to_string()),
            public_key: None,
            allowed_channels: Some(vec!["new-channel".to_string()]),
            allowed_guilds: None,
            admin_users: None,
            default_dm_user: Some("new-dm-user".to_string()),
        };

        db.set_discord_bot_config(&updated_config).await.unwrap();

        // Verify update
        let retrieved = db
            .get_discord_bot_config()
            .await
            .unwrap()
            .expect("config should exist");

        assert_eq!(retrieved.bot_token, "token-2");
        assert_eq!(retrieved.app_id, Some("new-app-id".to_string()));
        assert_eq!(
            retrieved.allowed_channels,
            Some(vec!["new-channel".to_string()])
        );
        assert_eq!(retrieved.default_dm_user, Some("new-dm-user".to_string()));
    }

    #[tokio::test]
    async fn test_discord_config_minimal() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Config with only required field
        let config = DiscordBotConfig {
            bot_token: "minimal-token".to_string(),
            app_id: None,
            public_key: None,
            allowed_channels: None,
            allowed_guilds: None,
            admin_users: None,
            default_dm_user: None,
        };

        db.set_discord_bot_config(&config).await.unwrap();

        let retrieved = db
            .get_discord_bot_config()
            .await
            .unwrap()
            .expect("config should exist");

        assert_eq!(retrieved.bot_token, "minimal-token");
        assert!(retrieved.app_id.is_none());
        assert!(retrieved.public_key.is_none());
        assert!(retrieved.allowed_channels.is_none());
        assert!(retrieved.allowed_guilds.is_none());
        assert!(retrieved.admin_users.is_none());
        assert!(retrieved.default_dm_user.is_none());
    }

    #[test]
    fn test_parse_comma_separated() {
        assert_eq!(
            parse_comma_separated("a,b,c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(
            parse_comma_separated("a, b , c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(parse_comma_separated("single"), vec!["single".to_string()]);
        assert_eq!(
            parse_comma_separated("a,,b"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(parse_comma_separated("").is_empty());
        assert!(parse_comma_separated(",,,").is_empty());
    }
}
