//! Migration audit models.
//!
//! Tracks v1 → v2 migration decisions and issues for debugging and rollback.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

/// Record of a v1 to v2 migration operation.
///
/// Each CAR file import creates an audit record tracking what was imported,
/// any issues found, and how they were resolved.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct MigrationAudit {
    /// Unique identifier
    pub id: String,

    /// When the import occurred
    pub imported_at: DateTime<Utc>,

    /// Source CAR file path
    pub source_file: String,

    /// Source format version
    pub source_version: i64,

    /// Number of issues detected during import
    pub issues_found: i64,

    /// Number of issues that were automatically resolved
    pub issues_resolved: i64,

    /// Full audit log as JSON
    /// Contains detailed record of:
    /// - Entities imported
    /// - Transformations applied
    /// - Issues and resolutions
    /// - Skipped items with reasons
    pub audit_log: Json<MigrationLog>,
}

/// Detailed migration log structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationLog {
    /// Summary statistics
    pub stats: MigrationStats,

    /// Individual entity import records
    pub entities: Vec<EntityImport>,

    /// Issues encountered during import
    pub issues: Vec<MigrationIssue>,
}

/// Migration statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationStats {
    /// Number of agents imported
    pub agents: i64,
    /// Number of memory blocks imported
    pub memory_blocks: i64,
    /// Number of messages imported
    pub messages: i64,
    /// Number of archival entries imported
    pub archival_entries: i64,
    /// Number of entities skipped
    pub skipped: i64,
    /// Total duration in milliseconds
    pub duration_ms: i64,
}

/// Record of a single entity import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityImport {
    /// Entity type (agent, memory_block, message, etc.)
    pub entity_type: String,
    /// Original v1 ID
    pub source_id: String,
    /// New v2 ID (may be same or different)
    pub target_id: String,
    /// Whether any transformation was applied
    pub transformed: bool,
    /// Description of transformation if applied
    pub transformation: Option<String>,
}

/// An issue encountered during migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationIssue {
    /// Issue severity
    pub severity: IssueSeverity,
    /// Entity type involved
    pub entity_type: Option<String>,
    /// Entity ID involved
    pub entity_id: Option<String>,
    /// Description of the issue
    pub description: String,
    /// How it was resolved (if at all)
    pub resolution: Option<String>,
    /// Whether the issue was automatically resolved
    pub auto_resolved: bool,
}

/// Migration issue severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    /// Informational, no action needed
    Info,
    /// Warning, migration continued but may need review
    Warning,
    /// Error, entity was skipped or partially imported
    Error,
    /// Critical, migration may be incomplete
    Critical,
}

impl Default for IssueSeverity {
    fn default() -> Self {
        Self::Info
    }
}
