//! Folder and file queries.

use chrono::Utc;
use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{FilePassage, Folder, FolderAccess, FolderAttachment, FolderFile};

// ============================================================================
// from_row implementations
// ============================================================================

impl Folder {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            path_type: row.get("path_type")?,
            path_value: row.get("path_value")?,
            embedding_model: row.get("embedding_model")?,
            created_at: row.get("created_at")?,
        })
    }
}

impl FolderFile {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            folder_id: row.get("folder_id")?,
            name: row.get("name")?,
            content_type: row.get("content_type")?,
            size_bytes: row.get("size_bytes")?,
            content: row.get("content")?,
            uploaded_at: row.get("uploaded_at")?,
            indexed_at: row.get("indexed_at")?,
        })
    }
}

impl FilePassage {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            file_id: row.get("file_id")?,
            content: row.get("content")?,
            start_line: row.get("start_line")?,
            end_line: row.get("end_line")?,
            chunk_index: row.get("chunk_index")?,
            created_at: row.get("created_at")?,
        })
    }
}

impl FolderAttachment {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            folder_id: row.get("folder_id")?,
            agent_id: row.get("agent_id")?,
            access: row.get("access")?,
            attached_at: row.get("attached_at")?,
        })
    }
}

// ============================================================================
// Folder CRUD
// ============================================================================

/// Create a new folder.
pub fn create_folder(conn: &rusqlite::Connection, folder: &Folder) -> DbResult<()> {
    conn.execute(
        "INSERT INTO folders (id, name, description, path_type, path_value, embedding_model, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![folder.id, folder.name, folder.description, folder.path_type, folder.path_value, folder.embedding_model, folder.created_at],
    )?;
    Ok(())
}

/// Get a folder by ID.
pub fn get_folder(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, path_type, path_value, embedding_model, created_at
         FROM folders WHERE id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], Folder::from_row)
        .optional()?;
    Ok(result)
}

/// Get a folder by name.
pub fn get_folder_by_name(conn: &rusqlite::Connection, name: &str) -> DbResult<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, path_type, path_value, embedding_model, created_at
         FROM folders WHERE name = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![name], Folder::from_row)
        .optional()?;
    Ok(result)
}

/// List all folders.
pub fn list_folders(conn: &rusqlite::Connection) -> DbResult<Vec<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, path_type, path_value, embedding_model, created_at
         FROM folders ORDER BY name",
    )?;
    let rows = stmt.query_map([], Folder::from_row)?;
    let mut folders = Vec::new();
    for row in rows {
        folders.push(row?);
    }
    Ok(folders)
}

/// Delete a folder (cascades to files and passages).
pub fn delete_folder(conn: &rusqlite::Connection, id: &str) -> DbResult<bool> {
    let count = conn.execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![id])?;
    Ok(count > 0)
}

// ============================================================================
// FolderFile CRUD
// ============================================================================

/// Create or update a file in a folder.
pub fn upsert_file(conn: &rusqlite::Connection, file: &FolderFile) -> DbResult<()> {
    conn.execute(
        "INSERT INTO folder_files (id, folder_id, name, content_type, size_bytes, content, uploaded_at, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(folder_id, name) DO UPDATE SET
             content_type = excluded.content_type,
             size_bytes = excluded.size_bytes,
             content = excluded.content,
             uploaded_at = excluded.uploaded_at",
        rusqlite::params![file.id, file.folder_id, file.name, file.content_type, file.size_bytes, file.content, file.uploaded_at, file.indexed_at],
    )?;
    Ok(())
}

/// Get a file by ID.
pub fn get_file(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<FolderFile>> {
    let mut stmt = conn.prepare(
        "SELECT id, folder_id, name, content_type, size_bytes, content, uploaded_at, indexed_at
         FROM folder_files WHERE id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], FolderFile::from_row)
        .optional()?;
    Ok(result)
}

/// Get a file by folder and name.
pub fn get_file_by_name(
    conn: &rusqlite::Connection,
    folder_id: &str,
    name: &str,
) -> DbResult<Option<FolderFile>> {
    let mut stmt = conn.prepare(
        "SELECT id, folder_id, name, content_type, size_bytes, content, uploaded_at, indexed_at
         FROM folder_files WHERE folder_id = ?1 AND name = ?2",
    )?;
    let result = stmt
        .query_row(rusqlite::params![folder_id, name], FolderFile::from_row)
        .optional()?;
    Ok(result)
}

/// List files in a folder.
pub fn list_files_in_folder(
    conn: &rusqlite::Connection,
    folder_id: &str,
) -> DbResult<Vec<FolderFile>> {
    let mut stmt = conn.prepare(
        "SELECT id, folder_id, name, content_type, size_bytes, content, uploaded_at, indexed_at
         FROM folder_files WHERE folder_id = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map(rusqlite::params![folder_id], FolderFile::from_row)?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

/// Mark a file as indexed.
pub fn mark_file_indexed(conn: &rusqlite::Connection, file_id: &str) -> DbResult<bool> {
    let now = Utc::now();
    let count = conn.execute(
        "UPDATE folder_files SET indexed_at = ?1 WHERE id = ?2",
        rusqlite::params![now, file_id],
    )?;
    Ok(count > 0)
}

/// Delete a file (cascades to passages).
pub fn delete_file(conn: &rusqlite::Connection, id: &str) -> DbResult<bool> {
    let count = conn.execute(
        "DELETE FROM folder_files WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(count > 0)
}

// ============================================================================
// FilePassage CRUD
// ============================================================================

/// Create a file passage.
pub fn create_passage(conn: &rusqlite::Connection, passage: &FilePassage) -> DbResult<()> {
    conn.execute(
        "INSERT INTO file_passages (id, file_id, content, start_line, end_line, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            passage.id,
            passage.file_id,
            passage.content,
            passage.start_line,
            passage.end_line,
            passage.created_at
        ],
    )?;
    Ok(())
}

/// Get passages for a file.
pub fn get_file_passages(conn: &rusqlite::Connection, file_id: &str) -> DbResult<Vec<FilePassage>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, content, start_line, end_line, chunk_index, created_at
         FROM file_passages WHERE file_id = ?1 ORDER BY chunk_index",
    )?;
    let rows = stmt.query_map(rusqlite::params![file_id], FilePassage::from_row)?;
    let mut passages = Vec::new();
    for row in rows {
        passages.push(row?);
    }
    Ok(passages)
}

/// Delete passages for a file (used before re-indexing).
pub fn delete_file_passages(conn: &rusqlite::Connection, file_id: &str) -> DbResult<u64> {
    let count = conn.execute(
        "DELETE FROM file_passages WHERE file_id = ?1",
        rusqlite::params![file_id],
    )?;
    Ok(count as u64)
}

// ============================================================================
// FolderAttachment (agent access)
// ============================================================================

/// Attach a folder to an agent.
pub fn attach_folder_to_agent(
    conn: &rusqlite::Connection,
    folder_id: &str,
    agent_id: &str,
    access: FolderAccess,
) -> DbResult<()> {
    let now = Utc::now();
    conn.execute(
        "INSERT INTO folder_attachments (folder_id, agent_id, access, attached_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(folder_id, agent_id) DO UPDATE SET access = excluded.access",
        rusqlite::params![folder_id, agent_id, access, now],
    )?;
    Ok(())
}

/// Detach a folder from an agent.
pub fn detach_folder_from_agent(
    conn: &rusqlite::Connection,
    folder_id: &str,
    agent_id: &str,
) -> DbResult<bool> {
    let count = conn.execute(
        "DELETE FROM folder_attachments WHERE folder_id = ?1 AND agent_id = ?2",
        rusqlite::params![folder_id, agent_id],
    )?;
    Ok(count > 0)
}

/// Get folders attached to an agent.
pub fn get_agent_folders(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<FolderAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT folder_id, agent_id, access, attached_at FROM folder_attachments WHERE agent_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], FolderAttachment::from_row)?;
    let mut attachments = Vec::new();
    for row in rows {
        attachments.push(row?);
    }
    Ok(attachments)
}

/// Get agents with access to a folder.
pub fn get_folder_agents(
    conn: &rusqlite::Connection,
    folder_id: &str,
) -> DbResult<Vec<FolderAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT folder_id, agent_id, access, attached_at FROM folder_attachments WHERE folder_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![folder_id], FolderAttachment::from_row)?;
    let mut attachments = Vec::new();
    for row in rows {
        attachments.push(row?);
    }
    Ok(attachments)
}
