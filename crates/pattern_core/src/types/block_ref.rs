//! Reference to a memory block for loading into agent context.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::memory::CONSTELLATION_OWNER;

/// Reference to a memory block for loading into context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema)]
pub struct BlockRef {
    /// Human-readable label for context display.
    pub label: String,
    /// Database block ID.
    pub block_id: String,
    /// Owner agent ID, defaults to "_constellation_" for shared blocks.
    pub agent_id: String,
}

impl BlockRef {
    /// Create a new block ref with constellation as default owner.
    pub fn new(label: impl Into<String>, block_id: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            block_id: block_id.into(),
            agent_id: CONSTELLATION_OWNER.to_string(),
        }
    }

    /// Create a block ref with explicit owner.
    pub fn with_owner(
        label: impl Into<String>,
        block_id: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            block_id: block_id.into(),
            agent_id: agent_id.into(),
        }
    }

    /// Set the owner agent ID (builder pattern).
    pub fn owned_by(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = agent_id.into();
        self
    }
}
