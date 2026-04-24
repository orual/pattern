//! Bridging conversions between `pattern_core` domain types and
//! `pattern_db` storage types.
//!
//! After the dependency inversion (`pattern_db` now depends on `pattern_core`),
//! most domain enums (MemoryPermission, MemoryBlockType, TaskStatus) are shared
//! directly — no conversion needed.
//!
//! This module retains:
//! - `SearchContentType` bridging (core and db use different variant names).
//! - `SearchResult` → `MemorySearchResult` projection (different struct shapes).
//! - `DbError` → `MemoryError` mapping helpers.

use pattern_core::error::MemoryError;
use pattern_core::types::memory_types::{MemorySearchResult, SearchContentType};
use pattern_db::DbError;
use pattern_db::search::{
    SearchContentType as DbSearchContentType, SearchResult as DbSearchResult,
};

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
