//! Bridging conversions between `pattern_core` domain types and
//! `pattern_db` storage types.
//!
//! These functions live here because `pattern_core` must not depend on
//! `pattern_db`, and `pattern_db` must not depend on `pattern_core`.
//! `pattern_memory` depends on both, making it the natural home for the
//! bridge.
//!
//! Orphan rules prevent `From` impls between two foreign types, so these
//! are free functions.

use pattern_core::error::MemoryError;
use pattern_core::types::memory_types::{
    BlockType, MemoryPermission, MemorySearchResult, SearchContentType,
};
use pattern_db::models::{MemoryBlockType, MemoryPermission as DbMemoryPermission};
use pattern_db::search::{SearchContentType as DbSearchContentType, SearchResult as DbSearchResult};
use pattern_db::DbError;

// ── MemoryPermission ↔ DbMemoryPermission ───────────────────────────────────

/// Convert core `MemoryPermission` to db `MemoryPermission`.
pub fn core_perm_to_db(p: MemoryPermission) -> DbMemoryPermission {
    match p {
        MemoryPermission::ReadOnly => DbMemoryPermission::ReadOnly,
        MemoryPermission::Partner => DbMemoryPermission::Partner,
        MemoryPermission::Human => DbMemoryPermission::Human,
        MemoryPermission::Append => DbMemoryPermission::Append,
        MemoryPermission::ReadWrite => DbMemoryPermission::ReadWrite,
        MemoryPermission::Admin => DbMemoryPermission::Admin,
    }
}

/// Convert db `MemoryPermission` to core `MemoryPermission`.
pub fn db_perm_to_core(p: DbMemoryPermission) -> MemoryPermission {
    match p {
        DbMemoryPermission::ReadOnly => MemoryPermission::ReadOnly,
        DbMemoryPermission::Partner => MemoryPermission::Partner,
        DbMemoryPermission::Human => MemoryPermission::Human,
        DbMemoryPermission::Append => MemoryPermission::Append,
        DbMemoryPermission::ReadWrite => MemoryPermission::ReadWrite,
        DbMemoryPermission::Admin => MemoryPermission::Admin,
    }
}

// ── BlockType ↔ MemoryBlockType ─────────────────────────────────────────────

/// Convert db `MemoryBlockType` to core `BlockType`.
pub fn db_block_type_to_core(t: MemoryBlockType) -> BlockType {
    match t {
        MemoryBlockType::Core => BlockType::Core,
        MemoryBlockType::Working => BlockType::Working,
        // Future-proofing: non-exhaustive requires a catch-all.
        _ => BlockType::Working,
    }
}

/// Convert core `BlockType` to db `MemoryBlockType`.
pub fn core_block_type_to_db(t: BlockType) -> MemoryBlockType {
    match t {
        BlockType::Core => MemoryBlockType::Core,
        BlockType::Working => MemoryBlockType::Working,
    }
}

// ── SearchContentType ↔ DbSearchContentType ─────────────────────────────────

/// Convert core `SearchContentType` to db `SearchContentType`.
pub fn core_search_type_to_db(ct: SearchContentType) -> DbSearchContentType {
    match ct {
        SearchContentType::Blocks => DbSearchContentType::MemoryBlock,
        SearchContentType::Archival => DbSearchContentType::ArchivalEntry,
        SearchContentType::Messages => DbSearchContentType::Message,
    }
}

/// Convert db `SearchContentType` to core `SearchContentType`.
pub fn db_search_type_to_core(ct: DbSearchContentType) -> SearchContentType {
    match ct {
        DbSearchContentType::Message => SearchContentType::Messages,
        DbSearchContentType::MemoryBlock => SearchContentType::Blocks,
        DbSearchContentType::ArchivalEntry => SearchContentType::Archival,
    }
}

// ── MemorySearchResult from DbSearchResult ──────────────────────────────────

/// Convert a db `SearchResult` to a core `MemorySearchResult`.
pub fn db_search_result_to_core(result: DbSearchResult) -> MemorySearchResult {
    MemorySearchResult {
        id: result.id,
        content_type: db_search_type_to_core(result.content_type),
        content: result.content,
        score: result.score,
    }
}

// ── DbError → MemoryError ───────────────────────────────────────────────────

/// Convert a `DbError` into a `MemoryError::Database` (string-mapped).
pub fn db_err_to_memory(e: DbError) -> MemoryError {
    MemoryError::Database(e.to_string())
}

/// Extension trait on `Result<T, DbError>` for ergonomic `?` conversion to
/// `MemoryResult<T>`.
pub trait DbResultExt<T> {
    /// Map a `DbError` to `MemoryError::Database` for `?` compatibility.
    fn mem(self) -> pattern_core::error::MemoryResult<T>;
}

impl<T> DbResultExt<T> for Result<T, DbError> {
    fn mem(self) -> pattern_core::error::MemoryResult<T> {
        self.map_err(db_err_to_memory)
    }
}

