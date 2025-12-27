//! Agent export/import functionality using DAG-CBOR CAR archives
//!
//! This module provides tools for exporting agents to portable CAR files
//! and importing them back, preserving all relationships and data.

mod exporter;
mod importer;
mod types;

pub use exporter::{AgentExporter, ExportOptions};
pub use importer::{AgentImporter, ImportOptions, ImportResult};

pub use types::{
    AgentExport, AgentRecordExport, ConstellationExport, ExportManifest, ExportStats, ExportType,
    GroupExport, MemoryChunk, MessageChunk,
};

/// Current export format version
pub const EXPORT_VERSION: u32 = 2;

/// Default chunk size for message batching
pub const DEFAULT_CHUNK_SIZE: usize = 1000;

/// Default chunk size for memory batching
pub const DEFAULT_MEMORY_CHUNK_SIZE: usize = 100;

/// Hard limit for any single block in a CAR file (bytes)
/// Keep at or below 1MB to maximize compatibility with common IPLD tooling.
pub const MAX_BLOCK_BYTES: usize = 1_000_000;
