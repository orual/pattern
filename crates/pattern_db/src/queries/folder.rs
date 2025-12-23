//! Folder and file queries.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::DbResult;
use crate::models::{
    FilePassage, Folder, FolderAccess, FolderAttachment, FolderFile, FolderPathType,
};

// ============================================================================
// Folder CRUD
// ============================================================================

/// Create a new folder.
pub async fn create_folder(pool: &SqlitePool, folder: &Folder) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO folders (id, name, description, path_type, path_value, embedding_model, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
        folder.id,
        folder.name,
        folder.description,
        folder.path_type,
        folder.path_value,
        folder.embedding_model,
        folder.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get a folder by ID.
pub async fn get_folder(pool: &SqlitePool, id: &str) -> DbResult<Option<Folder>> {
    let folder = sqlx::query_as!(
        Folder,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            path_type as "path_type!: FolderPathType",
            path_value,
            embedding_model as "embedding_model!",
            created_at as "created_at!: _"
        FROM folders WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(folder)
}

/// Get a folder by name.
pub async fn get_folder_by_name(pool: &SqlitePool, name: &str) -> DbResult<Option<Folder>> {
    let folder = sqlx::query_as!(
        Folder,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            path_type as "path_type!: FolderPathType",
            path_value,
            embedding_model as "embedding_model!",
            created_at as "created_at!: _"
        FROM folders WHERE name = ?
        "#,
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(folder)
}

/// List all folders.
pub async fn list_folders(pool: &SqlitePool) -> DbResult<Vec<Folder>> {
    let folders = sqlx::query_as!(
        Folder,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            path_type as "path_type!: FolderPathType",
            path_value,
            embedding_model as "embedding_model!",
            created_at as "created_at!: _"
        FROM folders ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(folders)
}

/// Delete a folder (cascades to files and passages).
pub async fn delete_folder(pool: &SqlitePool, id: &str) -> DbResult<bool> {
    let result = sqlx::query!("DELETE FROM folders WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

// ============================================================================
// FolderFile CRUD
// ============================================================================

/// Create or update a file in a folder.
pub async fn upsert_file(pool: &SqlitePool, file: &FolderFile) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO folder_files (id, folder_id, name, content_type, size_bytes, content, uploaded_at, indexed_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(folder_id, name) DO UPDATE SET
            content_type = excluded.content_type,
            size_bytes = excluded.size_bytes,
            content = excluded.content,
            uploaded_at = excluded.uploaded_at
        "#,
        file.id,
        file.folder_id,
        file.name,
        file.content_type,
        file.size_bytes,
        file.content,
        file.uploaded_at,
        file.indexed_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get a file by ID.
pub async fn get_file(pool: &SqlitePool, id: &str) -> DbResult<Option<FolderFile>> {
    let file = sqlx::query_as!(
        FolderFile,
        r#"
        SELECT
            id as "id!",
            folder_id as "folder_id!",
            name as "name!",
            content_type,
            size_bytes,
            content,
            uploaded_at as "uploaded_at!: _",
            indexed_at as "indexed_at: _"
        FROM folder_files WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(file)
}

/// Get a file by folder and name.
pub async fn get_file_by_name(
    pool: &SqlitePool,
    folder_id: &str,
    name: &str,
) -> DbResult<Option<FolderFile>> {
    let file = sqlx::query_as!(
        FolderFile,
        r#"
        SELECT
            id as "id!",
            folder_id as "folder_id!",
            name as "name!",
            content_type,
            size_bytes,
            content,
            uploaded_at as "uploaded_at!: _",
            indexed_at as "indexed_at: _"
        FROM folder_files WHERE folder_id = ? AND name = ?
        "#,
        folder_id,
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(file)
}

/// List files in a folder.
pub async fn list_files_in_folder(pool: &SqlitePool, folder_id: &str) -> DbResult<Vec<FolderFile>> {
    let files = sqlx::query_as!(
        FolderFile,
        r#"
        SELECT
            id as "id!",
            folder_id as "folder_id!",
            name as "name!",
            content_type,
            size_bytes,
            content,
            uploaded_at as "uploaded_at!: _",
            indexed_at as "indexed_at: _"
        FROM folder_files WHERE folder_id = ? ORDER BY name
        "#,
        folder_id
    )
    .fetch_all(pool)
    .await?;
    Ok(files)
}

/// Mark a file as indexed.
pub async fn mark_file_indexed(pool: &SqlitePool, file_id: &str) -> DbResult<bool> {
    let now = Utc::now();
    let result = sqlx::query!(
        "UPDATE folder_files SET indexed_at = ? WHERE id = ?",
        now,
        file_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Delete a file (cascades to passages).
pub async fn delete_file(pool: &SqlitePool, id: &str) -> DbResult<bool> {
    let result = sqlx::query!("DELETE FROM folder_files WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

// ============================================================================
// FilePassage CRUD
// ============================================================================

/// Create a file passage.
pub async fn create_passage(pool: &SqlitePool, passage: &FilePassage) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO file_passages (id, file_id, content, start_line, end_line, created_at)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
        passage.id,
        passage.file_id,
        passage.content,
        passage.start_line,
        passage.end_line,
        passage.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get passages for a file.
pub async fn get_file_passages(pool: &SqlitePool, file_id: &str) -> DbResult<Vec<FilePassage>> {
    let passages = sqlx::query_as!(
        FilePassage,
        r#"
        SELECT
            id as "id!",
            file_id as "file_id!",
            content as "content!",
            start_line,
            end_line,
            chunk_index as "chunk_index!",
            created_at as "created_at!: _"
        FROM file_passages WHERE file_id = ? ORDER BY chunk_index
        "#,
        file_id
    )
    .fetch_all(pool)
    .await?;
    Ok(passages)
}

/// Delete passages for a file (used before re-indexing).
pub async fn delete_file_passages(pool: &SqlitePool, file_id: &str) -> DbResult<u64> {
    let result = sqlx::query!("DELETE FROM file_passages WHERE file_id = ?", file_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

// ============================================================================
// FolderAttachment (agent access)
// ============================================================================

/// Attach a folder to an agent.
pub async fn attach_folder_to_agent(
    pool: &SqlitePool,
    folder_id: &str,
    agent_id: &str,
    access: FolderAccess,
) -> DbResult<()> {
    let now = Utc::now();
    sqlx::query!(
        r#"
        INSERT INTO folder_attachments (folder_id, agent_id, access, attached_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(folder_id, agent_id) DO UPDATE SET access = excluded.access
        "#,
        folder_id,
        agent_id,
        access,
        now,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Detach a folder from an agent.
pub async fn detach_folder_from_agent(
    pool: &SqlitePool,
    folder_id: &str,
    agent_id: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "DELETE FROM folder_attachments WHERE folder_id = ? AND agent_id = ?",
        folder_id,
        agent_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Get folders attached to an agent.
pub async fn get_agent_folders(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<Vec<FolderAttachment>> {
    let attachments = sqlx::query_as!(
        FolderAttachment,
        r#"
        SELECT
            folder_id as "folder_id!",
            agent_id as "agent_id!",
            access as "access!: FolderAccess",
            attached_at as "attached_at!: _"
        FROM folder_attachments WHERE agent_id = ?
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(attachments)
}

/// Get agents with access to a folder.
pub async fn get_folder_agents(
    pool: &SqlitePool,
    folder_id: &str,
) -> DbResult<Vec<FolderAttachment>> {
    let attachments = sqlx::query_as!(
        FolderAttachment,
        r#"
        SELECT
            folder_id as "folder_id!",
            agent_id as "agent_id!",
            access as "access!: FolderAccess",
            attached_at as "attached_at!: _"
        FROM folder_attachments WHERE folder_id = ?
        "#,
        folder_id
    )
    .fetch_all(pool)
    .await?;
    Ok(attachments)
}
