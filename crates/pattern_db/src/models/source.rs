//! Data source models.
//!
//! Data sources represent external integrations that feed content into the constellation:
//! - File watchers
//! - Discord channels
//! - Bluesky feeds
//! - RSS feeds
//! - etc.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

/// A configured data source.
///
/// Data sources can push content into the constellation, which gets
/// routed to subscribed agents based on notification templates.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct DataSource {
    /// Unique identifier
    pub id: String,

    /// Human-readable name (unique within constellation)
    pub name: String,

    /// Type of data source
    pub source_type: SourceType,

    /// Source-specific configuration as JSON
    /// Contents vary by source_type:
    /// - file: { path, patterns, recursive }
    /// - discord: { guild_id, channel_ids, event_types }
    /// - bluesky: { dids, lists, feeds }
    /// - rss: { urls, poll_interval }
    pub config: Json<serde_json::Value>,

    /// When the source was last synced
    pub last_sync_at: Option<DateTime<Utc>>,

    /// Source-specific position marker for incremental sync
    /// - file: last modified timestamp or inode
    /// - discord: last message snowflake
    /// - bluesky: cursor from firehose
    /// - rss: ETag or last-modified header
    pub sync_cursor: Option<String>,

    /// Whether the source is enabled
    pub enabled: bool,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

/// Types of data sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    // ===== File & Code =====
    /// Local or remote file watching
    File,
    /// Version control events (Git, Jujutsu, Mercurial, etc.)
    Vcs,
    /// Code hosting platforms (GitHub, GitLab, Forgejo, etc.)
    CodeHost,
    /// Language Server Protocol events (diagnostics, completions)
    LanguageServer,
    /// Terminal/shell output capture
    Terminal,

    // ===== Communication =====
    /// Group chat platforms (Discord servers, Slack workspaces, etc.)
    GroupChat,
    /// Direct messaging (Discord DMs, etc.)
    DirectChat,
    /// Bluesky/ATProto firehose or feed
    Bluesky,
    /// Email (IMAP/SMTP)
    Email,

    // ===== Scheduling & Time =====
    /// Calendar integration (Google Calendar, iCal, etc.)
    Calendar,
    /// Scheduled/periodic triggers (pomodoro, reminders)
    Timer,

    // ===== Integration =====
    /// MCP server as data source
    Mcp,
    /// Agent-to-agent notifications (supervisor patterns)
    Agent,
    /// Generic HTTP polling (RSS, Atom, JSON APIs)
    Http,
    /// Webhook receiver
    Webhook,
    /// Manual/API push
    Manual,
}

impl std::fmt::Display for SourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File => write!(f, "file"),
            Self::Vcs => write!(f, "vcs"),
            Self::CodeHost => write!(f, "code_host"),
            Self::LanguageServer => write!(f, "language_server"),
            Self::Terminal => write!(f, "terminal"),
            Self::GroupChat => write!(f, "group_chat"),
            Self::DirectChat => write!(f, "direct_chat"),
            Self::Bluesky => write!(f, "bluesky"),
            Self::Email => write!(f, "email"),
            Self::Calendar => write!(f, "calendar"),
            Self::Timer => write!(f, "timer"),
            Self::Mcp => write!(f, "mcp"),
            Self::Agent => write!(f, "agent"),
            Self::Http => write!(f, "http"),
            Self::Webhook => write!(f, "webhook"),
            Self::Manual => write!(f, "manual"),
        }
    }
}

/// Subscription linking an agent to a data source.
///
/// When the data source receives content, it gets formatted using
/// the notification template and sent to the agent.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentDataSource {
    /// Agent receiving notifications
    pub agent_id: String,

    /// Data source providing content
    pub source_id: String,

    /// Template for formatting notifications
    /// Uses mustache-style placeholders: {{content}}, {{source}}, {{timestamp}}
    /// If None, uses a default template based on source type
    pub notification_template: Option<String>,
}
