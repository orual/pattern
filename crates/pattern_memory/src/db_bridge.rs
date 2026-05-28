// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use pattern_core::types::memory_types::{MemorySearchResult, SearchContentType, SearchHit};
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

/// Convert a db `SearchResult` to a core `MemorySearchResult`, using `resolve_block`
/// to translate a block-hit's DB row id into its (scope, label) address. Caller
/// (typically `MemoryCache::search_impl`) provides the resolver from its in-memory
/// block index; if the block isn't cached the resolver may return `None`, in which
/// case the hit is rendered as `SearchHit::Block` with a fallback empty scope+label
/// (caller should warn — this indicates a cold-search hit on an uncached block).
pub fn db_search_result_to_core(
    result: DbSearchResult,
    resolve_block: impl Fn(&str) -> Option<(pattern_core::types::memory_types::Scope, smol_str::SmolStr)>,
) -> MemorySearchResult {
    let content_type = db_search_type_to_core(result.content_type);
    let hit = match content_type {
        SearchContentType::Blocks => {
            if let Some((scope, label)) = resolve_block(&result.id) {
                SearchHit::Block { scope, label }
            } else {
                // Cold hit — block isn't in cache. Caller logs; we emit an empty
                // addr that round-trips but won't resolve client-side. Better than
                // panicking; downstream display can still show snippet+score.
                SearchHit::Block {
                    scope: pattern_core::types::memory_types::Scope::global(""),
                    label: smol_str::SmolStr::from(result.id.as_str()),
                }
            }
        }
        SearchContentType::Archival => SearchHit::Archival { entry_id: result.id },
        SearchContentType::Messages => SearchHit::Message { message_id: result.id },
    };
    MemorySearchResult {
        hit,
        content_type,
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
