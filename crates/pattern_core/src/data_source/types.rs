//! Core types for the data source system.
//!
//! This module defines the foundational types used by both DataStream
//! (event-driven) and DataBlock (document-oriented) sources.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::SnowflakePosition;
use crate::memory::BlockSchema;
use crate::messages::Message;

// Re-export BlockRef from messages for convenience
pub use crate::messages::BlockRef;

/// Notification delivered to agent via broadcast channel
#[derive(Debug, Clone)]
pub struct Notification {
    /// Full Message type - supports text, images, multi-modal content
    pub message: Message,
    /// Blocks to load for this batch (already exist in memory store)
    pub block_refs: Vec<BlockRef>,
    /// Batch to associate these blocks with
    pub batch_id: SnowflakePosition,
}

impl Notification {
    /// Create a notification with no block references
    pub fn new(message: Message, batch_id: SnowflakePosition) -> Self {
        Self {
            message,
            block_refs: Vec::new(),
            batch_id,
        }
    }

    /// Add block references to this notification
    pub fn with_blocks(mut self, block_refs: Vec<BlockRef>) -> Self {
        self.block_refs = block_refs;
        self
    }
}

/// Opaque cursor for pull-based stream access
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamCursor(pub String);

impl Default for StreamCursor {
    fn default() -> Self {
        Self(String::new())
    }
}

impl StreamCursor {
    pub fn new(cursor: impl Into<String>) -> Self {
        Self(cursor.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Schema specification for blocks a source creates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSchemaSpec {
    /// Label pattern: exact "lsp_diagnostics" or templated "bluesky_user_{handle}"
    pub label_pattern: String,
    /// Schema definition
    pub schema: BlockSchema,
    /// Human-readable description
    pub description: String,
    /// Whether blocks are created pinned (always in context) or ephemeral
    pub pinned: bool,
}

impl BlockSchemaSpec {
    pub fn pinned(
        label: impl Into<String>,
        schema: BlockSchema,
        description: impl Into<String>,
    ) -> Self {
        Self {
            label_pattern: label.into(),
            schema,
            description: description.into(),
            pinned: true,
        }
    }

    pub fn ephemeral(
        label_pattern: impl Into<String>,
        schema: BlockSchema,
        description: impl Into<String>,
    ) -> Self {
        Self {
            label_pattern: label_pattern.into(),
            schema,
            description: description.into(),
            pinned: false,
        }
    }
}

/// Internal event from streaming source (before formatting)
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub event_type: String,
    pub payload: serde_json::Value,
    pub cursor: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub source_id: String,
}

impl StreamEvent {
    pub fn new(
        source_id: impl Into<String>,
        event_type: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            event_type: event_type.into(),
            payload,
            cursor: None,
            timestamp: Utc::now(),
        }
    }
}
