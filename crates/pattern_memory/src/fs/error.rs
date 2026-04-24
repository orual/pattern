//! Shared error types for the filesystem serialization layer.
//!
//! All format modules (`markdown`, `kdl`, `jsonl`) surface errors through
//! [`FsError`], which also wraps [`KdlConversionError`] for the KDL converter.

use std::path::PathBuf;

use pattern_core::types::memory_types::BlockSchemaKind;

use crate::fs::kdl::KdlConversionError;

/// Errors arising from filesystem serialization and deserialization of memory
/// blocks.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FsError {
    /// An I/O error reading or writing a block file.
    #[error("io error reading/writing block file at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The file content could not be parsed for the expected format.
    #[error("invalid file format for {path}: {reason}")]
    ParseError { path: PathBuf, reason: String },

    /// A KDL conversion error (forward or reverse).
    #[error(transparent)]
    KdlConversion(#[from] KdlConversionError),

    /// A JSON serialization/deserialization error.
    #[error(transparent)]
    JsonLine(#[from] serde_json::Error),

    /// UTF-8 decoding failed when reading a file.
    #[error("UTF-8 error reading {path}: {source}")]
    Utf8 {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },

    /// A converter for this block schema kind has not been implemented yet.
    ///
    /// This is a typed placeholder for schema variants whose filesystem
    /// serializer/deserializer is planned but not yet landed. Task 7 of
    /// Phase 4 replaces the `Skill` arm that returns this error with the
    /// real `markdown_skill::emit` / `markdown_skill::parse` calls.
    #[error("no filesystem converter available yet for schema kind {0:?}")]
    ConverterNotYetAvailable(BlockSchemaKind),
}
