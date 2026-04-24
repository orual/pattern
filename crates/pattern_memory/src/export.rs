//! CAR archive export/import for Pattern agents and constellations.
//!
//! Format version 3 - designed for SQLite-backed architecture.
//!
//! Relocated from `pattern_core::export` to `pattern_memory::export` to
//! eliminate the `pattern_core` -> `pattern_db` circular dependency.
//! This module naturally belongs here since it bridges core domain types
//! and database storage types.

mod car;
mod exporter;
mod importer;
pub mod letta_convert;
pub mod letta_types;
pub mod types;

#[cfg(test)]
mod tests;

pub use car::*;
pub use exporter::*;
pub use importer::*;
pub use letta_convert::{
    LettaConversionError, LettaConversionOptions, LettaConversionStats, convert_letta_to_car,
};
pub use letta_types::AgentFileSchema;
pub use types::*;

/// Export format version.
pub const EXPORT_VERSION: u32 = 3;

/// Maximum bytes per CAR block (IPLD compatibility).
pub const MAX_BLOCK_BYTES: usize = 1_000_000;

/// Default max messages per chunk.
pub const DEFAULT_MAX_MESSAGES_PER_CHUNK: usize = 1000;

/// Target bytes per chunk (leave headroom under MAX_BLOCK_BYTES).
pub const TARGET_CHUNK_BYTES: usize = 900_000;

/// Extension trait for `?`-converting `DbError` to `CoreError::SqliteError`.
pub(crate) trait DbToCoreExt<T> {
    fn db(self) -> pattern_core::error::Result<T>;
}

impl<T> DbToCoreExt<T> for Result<T, pattern_db::DbError> {
    fn db(self) -> pattern_core::error::Result<T> {
        self.map_err(|e| pattern_core::error::CoreError::SqliteError(e.to_string()))
    }
}
