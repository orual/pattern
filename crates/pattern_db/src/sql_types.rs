// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! `FromSql`/`ToSql` implementations for domain enum types stored as TEXT
//! columns in SQLite.
//!
//! Each enum that appears in a SQLite column needs these impls for rusqlite
//! to bind and extract values. All TEXT-encoded enums use their canonical
//! database string form (typically snake_case).
//!
//! # Timestamp storage
//!
//! `jiff::Timestamp` fields (used in message-related models) are stored as
//! RFC 3339 UTC strings (e.g. `"2026-04-19T12:00:00.000000000Z"`). Because
//! the orphan rule prevents implementing rusqlite's `ToSql`/`FromSql` for
//! `jiff::Timestamp` directly, the conversion is done explicitly in each
//! query function (`from_row` reads the TEXT column and parses it;
//! `create_message` etc. call `.to_string()` when binding). See
//! `queries/message.rs` and `queries/queue.rs` for the concrete conversions.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

/// Implement `ToSql` and `FromSql` for an enum that has `as_str()` -> db format
/// and `FromStr` that parses the db format.
macro_rules! impl_text_sql_via_as_str {
    ($ty:ty) => {
        impl ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $ty {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let s = value.as_str()?;
                s.parse::<Self>().map_err(|e| {
                    FromSqlError::Other(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e.to_string(),
                    )))
                })
            }
        }
    };
}

/// Implement `ToSql` and `FromSql` for an enum that has `Display` producing
/// the db format and `FromStr` that parses it.
macro_rules! impl_text_sql_via_display {
    ($ty:ty) => {
        impl ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.to_string()))
            }
        }

        impl FromSql for $ty {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let s = value.as_str()?;
                s.parse::<Self>().map_err(|e| {
                    FromSqlError::Other(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e.to_string(),
                    )))
                })
            }
        }
    };
}

// --- Memory types ---
// MemoryBlockType, MemoryPermission, TaskStatus: FromSql/ToSql impls live in
// pattern_core::types::sql_types (behind the `sqlite` feature). They are
// available here because pattern_db enables that feature.

// --- Message types ---
// MessageRole: Display produces "user"/"assistant"/"system"/"tool" which matches db.
impl std::str::FromStr for crate::models::MessageRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            "tool" => Ok(Self::Tool),
            _ => Err(format!("unknown message role '{s}'")),
        }
    }
}

impl_text_sql_via_display!(crate::models::MessageRole);

// BatchType: stored as snake_case. Need Display and FromStr.
impl std::fmt::Display for crate::models::BatchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserRequest => write!(f, "user_request"),
            Self::AgentToAgent => write!(f, "agent_to_agent"),
            Self::SystemTrigger => write!(f, "system_trigger"),
            Self::Continuation => write!(f, "continuation"),
        }
    }
}

impl std::str::FromStr for crate::models::BatchType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user_request" => Ok(Self::UserRequest),
            "agent_to_agent" => Ok(Self::AgentToAgent),
            "system_trigger" => Ok(Self::SystemTrigger),
            "continuation" => Ok(Self::Continuation),
            _ => Err(format!("unknown batch type '{s}'")),
        }
    }
}

impl_text_sql_via_display!(crate::models::BatchType);

// --- Agent types ---
impl std::fmt::Display for crate::models::AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Hibernated => write!(f, "hibernated"),
            Self::Archived => write!(f, "archived"),
        }
    }
}

impl std::str::FromStr for crate::models::AgentStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "hibernated" => Ok(Self::Hibernated),
            "archived" => Ok(Self::Archived),
            _ => Err(format!("unknown agent status '{s}'")),
        }
    }
}

impl_text_sql_via_display!(crate::models::AgentStatus);

// --- Event types ---
impl std::fmt::Display for crate::models::OccurrenceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scheduled => write!(f, "scheduled"),
            Self::Active => write!(f, "active"),
            Self::Completed => write!(f, "completed"),
            Self::Skipped => write!(f, "skipped"),
            Self::Snoozed => write!(f, "snoozed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::str::FromStr for crate::models::OccurrenceStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "scheduled" => Ok(Self::Scheduled),
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "skipped" => Ok(Self::Skipped),
            "snoozed" => Ok(Self::Snoozed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("unknown occurrence status '{s}'")),
        }
    }
}

impl_text_sql_via_display!(crate::models::OccurrenceStatus);

// --- Folder types ---
impl std::str::FromStr for crate::models::FolderPathType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "local" => Ok(Self::Local),
            "virtual" => Ok(Self::Virtual),
            "remote" => Ok(Self::Remote),
            _ => Err(format!("unknown folder path type '{s}'")),
        }
    }
}

// FolderPathType Display produces db format.
impl_text_sql_via_display!(crate::models::FolderPathType);

impl std::str::FromStr for crate::models::FolderAccess {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "read_write" => Ok(Self::ReadWrite),
            _ => Err(format!("unknown folder access '{s}'")),
        }
    }
}

// FolderAccess Display produces db format.
impl_text_sql_via_display!(crate::models::FolderAccess);

// --- Source types ---
// SourceType Display already produces db format (snake_case).
impl std::str::FromStr for crate::models::SourceType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "file" => Ok(Self::File),
            "vcs" => Ok(Self::Vcs),
            "code_host" => Ok(Self::CodeHost),
            "language_server" => Ok(Self::LanguageServer),
            "terminal" => Ok(Self::Terminal),
            "group_chat" => Ok(Self::GroupChat),
            "direct_chat" => Ok(Self::DirectChat),
            "bluesky" => Ok(Self::Bluesky),
            "email" => Ok(Self::Email),
            "calendar" => Ok(Self::Calendar),
            "timer" => Ok(Self::Timer),
            "mcp" => Ok(Self::Mcp),
            "agent" => Ok(Self::Agent),
            "http" => Ok(Self::Http),
            "webhook" => Ok(Self::Webhook),
            "manual" => Ok(Self::Manual),
            // Legacy aliases.
            "discord" => Ok(Self::GroupChat),
            "rss" => Ok(Self::Http),
            "api" => Ok(Self::Http),
            "process" => Ok(Self::Terminal),
            _ => Err(format!("unknown source type '{s}'")),
        }
    }
}

impl_text_sql_via_display!(crate::models::SourceType);

// --- TaskList block-index types ---
// TaskStatus: FromSql/ToSql impl lives in pattern_core::types::sql_types.

// --- Task (ADHD) types ---
// UserTaskStatus: Display produces "in progress" (human-readable) but db wants "in_progress".
// Need dedicated as_str().
impl crate::models::UserTaskStatus {
    /// Database-format string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Deferred => "deferred",
        }
    }
}

impl std::str::FromStr for crate::models::UserTaskStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "backlog" => Ok(Self::Backlog),
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "blocked" => Ok(Self::Blocked),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "deferred" => Ok(Self::Deferred),
            _ => Err(format!("unknown user task status '{s}'")),
        }
    }
}

impl_text_sql_via_as_str!(crate::models::UserTaskStatus);

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    /// Generic round-trip test: insert via ToSql, verify stored text, read via FromSql.
    fn round_trip<T>(value: T, expected_text: &str)
    where
        T: rusqlite::types::ToSql + rusqlite::types::FromSql + std::fmt::Debug + PartialEq,
    {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (v TEXT)", []).unwrap();
        conn.execute("INSERT INTO t (v) VALUES (?1)", [&value])
            .unwrap();

        let stored: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(stored, expected_text, "stored text mismatch for {value:?}");

        let loaded: T = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(loaded, value, "round-trip mismatch");
    }

    /// Test that garbage input produces a useful error.
    fn reject_garbage<T>(garbage: &str)
    where
        T: rusqlite::types::FromSql + std::fmt::Debug,
    {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (v TEXT)", []).unwrap();
        conn.execute("INSERT INTO t (v) VALUES (?1)", [garbage])
            .unwrap();

        let result = conn.query_row("SELECT v FROM t", [], |r| r.get::<_, T>(0));
        assert!(result.is_err(), "expected error for garbage '{garbage}'");
    }

    use crate::models::*;

    #[test]
    fn memory_block_type_round_trip() {
        round_trip(MemoryBlockType::Core, "core");
        round_trip(MemoryBlockType::Working, "working");
    }

    #[test]
    fn memory_block_type_rejects_garbage() {
        reject_garbage::<MemoryBlockType>("nonsense");
    }

    #[test]
    fn memory_block_type_rejects_removed_variants() {
        reject_garbage::<MemoryBlockType>("archival");
        reject_garbage::<MemoryBlockType>("log");
    }

    #[test]
    fn memory_permission_round_trip() {
        round_trip(MemoryPermission::ReadOnly, "read_only");
        round_trip(MemoryPermission::Partner, "partner");
        round_trip(MemoryPermission::Admin, "admin");
        round_trip(MemoryPermission::ReadWrite, "read_write");
    }

    #[test]
    fn message_role_round_trip() {
        round_trip(MessageRole::User, "user");
        round_trip(MessageRole::Assistant, "assistant");
        round_trip(MessageRole::System, "system");
        round_trip(MessageRole::Tool, "tool");
    }

    #[test]
    fn message_role_rejects_garbage() {
        reject_garbage::<MessageRole>("moderator");
    }

    #[test]
    fn agent_status_round_trip() {
        round_trip(AgentStatus::Active, "active");
        round_trip(AgentStatus::Hibernated, "hibernated");
        round_trip(AgentStatus::Archived, "archived");
    }

    #[test]
    fn batch_type_round_trip() {
        round_trip(BatchType::UserRequest, "user_request");
        round_trip(BatchType::AgentToAgent, "agent_to_agent");
        round_trip(BatchType::SystemTrigger, "system_trigger");
        round_trip(BatchType::Continuation, "continuation");
    }

    #[test]
    fn occurrence_status_round_trip() {
        round_trip(OccurrenceStatus::Scheduled, "scheduled");
        round_trip(OccurrenceStatus::Active, "active");
        round_trip(OccurrenceStatus::Snoozed, "snoozed");
        round_trip(OccurrenceStatus::Cancelled, "cancelled");
    }

    #[test]
    fn folder_types_round_trip() {
        round_trip(FolderPathType::Local, "local");
        round_trip(FolderPathType::Virtual, "virtual");
        round_trip(FolderPathType::Remote, "remote");
        round_trip(FolderAccess::Read, "read");
        round_trip(FolderAccess::ReadWrite, "read_write");
    }

    #[test]
    fn source_type_round_trip() {
        round_trip(SourceType::Bluesky, "bluesky");
        round_trip(SourceType::Terminal, "terminal");
        round_trip(SourceType::Mcp, "mcp");
    }

    #[test]
    fn user_task_status_round_trip() {
        round_trip(UserTaskStatus::Backlog, "backlog");
        round_trip(UserTaskStatus::InProgress, "in_progress");
        round_trip(UserTaskStatus::Blocked, "blocked");
        round_trip(UserTaskStatus::Deferred, "deferred");
    }
}
