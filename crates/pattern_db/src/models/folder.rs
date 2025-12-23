//! Folder and file models.
//!
//! Manages file access for agents with semantic search over file contents.
//! Files are chunked into passages for embedding and retrieval.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// A folder containing files accessible to agents.
///
/// Folders can be:
/// - Local filesystem paths
/// - Virtual (content stored in DB)
/// - Remote (URLs, cloud storage)
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Folder {
    /// Unique identifier
    pub id: String,

    /// Human-readable name (unique within constellation)
    pub name: String,

    /// Description of folder contents/purpose
    pub description: Option<String>,

    /// Type of folder path
    pub path_type: FolderPathType,

    /// Actual path or URL (interpretation depends on path_type)
    pub path_value: Option<String>,

    /// Embedding model used for this folder's passages
    pub embedding_model: String,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

/// Folder path types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum FolderPathType {
    /// Local filesystem path
    Local,
    /// Content stored in database (no external path)
    Virtual,
    /// Remote URL or cloud storage path
    Remote,
}

impl std::fmt::Display for FolderPathType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Virtual => write!(f, "virtual"),
            Self::Remote => write!(f, "remote"),
        }
    }
}

/// A file within a folder.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct FolderFile {
    /// Unique identifier
    pub id: String,

    /// Parent folder
    pub folder_id: String,

    /// Filename (unique within folder)
    pub name: String,

    /// MIME type
    pub content_type: Option<String>,

    /// File size in bytes
    pub size_bytes: Option<i64>,

    /// File content (for virtual folders)
    pub content: Option<Vec<u8>>,

    /// When the file was uploaded/detected
    pub uploaded_at: DateTime<Utc>,

    /// When the file was last indexed (passages generated)
    pub indexed_at: Option<DateTime<Utc>>,
}

/// A passage (chunk) of a file for semantic search.
///
/// Files are split into passages for embedding. Passages are the unit
/// of retrieval - when an agent searches, they get relevant passages.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct FilePassage {
    /// Unique identifier
    pub id: String,

    /// Parent file
    pub file_id: String,

    /// Passage content (text chunk)
    pub content: String,

    /// Starting line in source file (for code files)
    pub start_line: Option<i64>,

    /// Ending line in source file
    pub end_line: Option<i64>,

    /// Chunk index within file (0-based)
    pub chunk_index: i64,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

/// Attachment linking a folder to an agent.
///
/// Determines what access level an agent has to a folder's files.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct FolderAttachment {
    /// Folder being attached
    pub folder_id: String,

    /// Agent gaining access
    pub agent_id: String,

    /// Access level
    pub access: FolderAccess,

    /// When the attachment was created
    pub attached_at: DateTime<Utc>,
}

/// Folder access levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum FolderAccess {
    /// Can read files but not modify
    Read,
    /// Can read and write files
    ReadWrite,
}

impl Default for FolderAccess {
    fn default() -> Self {
        Self::Read
    }
}

impl std::fmt::Display for FolderAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::ReadWrite => write!(f, "read_write"),
        }
    }
}
