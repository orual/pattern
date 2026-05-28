// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Metadata types for memory blocks, archival entries, and shared blocks.
//!
//! These types appear in [`crate::traits::MemoryStore`] method return types
//! and are shared across crate boundaries.

use jiff::Timestamp;
use serde_json::Value as JsonValue;

use super::{BlockSchema, MemoryBlockType};

/// Block metadata (without loading the full document).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockMetadata {
    pub id: String,
    pub agent_id: String,
    pub label: String,
    pub description: String,
    pub block_type: MemoryBlockType,
    pub schema: BlockSchema,
    pub char_limit: usize,
    pub permission: super::MemoryPermission,
    pub pinned: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl BlockMetadata {
    /// Create standalone metadata for testing or documents not backed by DB.
    pub fn standalone(schema: BlockSchema) -> Self {
        let now = Timestamp::now();
        Self {
            id: String::new(),
            agent_id: String::new(),
            label: String::new(),
            description: String::new(),
            block_type: MemoryBlockType::Working,
            schema,
            char_limit: 0,
            permission: super::MemoryPermission::ReadWrite,
            pinned: false,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Archival entry (for search results).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchivalEntry {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub metadata: Option<JsonValue>,
    pub created_at: Timestamp,
}

/// Information about a block shared with an agent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SharedBlockInfo {
    pub block_id: String,
    pub owner_agent_id: String,
    /// The display name of the owning agent (if available).
    pub owner_agent_name: Option<String>,
    pub label: String,
    pub description: String,
    pub block_type: MemoryBlockType,
    pub permission: super::MemoryPermission,
}
