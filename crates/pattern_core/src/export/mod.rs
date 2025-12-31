//! CAR archive export/import for Pattern agents and constellations.
//!
//! Format version 3 - designed for SQLite-backed architecture.

mod car;
mod exporter;
mod importer;
pub mod letta_convert;
pub mod letta_types;
mod types;

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

/// Export format version
pub const EXPORT_VERSION: u32 = 3;

/// Maximum bytes per CAR block (IPLD compatibility)
pub const MAX_BLOCK_BYTES: usize = 1_000_000;

/// Default max messages per chunk
pub const DEFAULT_MAX_MESSAGES_PER_CHUNK: usize = 1000;

/// Target bytes per chunk (leave headroom under MAX_BLOCK_BYTES)
pub const TARGET_CHUNK_BYTES: usize = 900_000;
