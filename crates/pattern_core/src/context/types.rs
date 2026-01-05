//! Types for v2 context building

use crate::memory::BlockType;
use std::collections::HashMap;

// Re-export the real CompressionStrategy from context/compression.rs
pub use crate::context::compression::CompressionStrategy;

/// Model-specific context limits
#[derive(Debug, Clone)]
pub struct ModelContextLimits {
    pub max_tokens: usize,
    pub memory_tokens: usize,
    pub history_tokens: usize,
    pub reserved_response_tokens: usize,
}

impl ModelContextLimits {
    pub fn large() -> Self {
        Self {
            max_tokens: 200_000,
            memory_tokens: 12_000,
            history_tokens: 80_000,
            reserved_response_tokens: 8_000,
        }
    }

    pub fn small() -> Self {
        Self {
            max_tokens: 200_000,
            memory_tokens: 6_000,
            history_tokens: 40_000,
            reserved_response_tokens: 4_000,
        }
    }
}

/// Configuration for context building
#[derive(Debug, Clone)]
pub struct ContextConfig {
    pub default_limits: ModelContextLimits,
    pub model_overrides: HashMap<String, ModelContextLimits>,
    pub include_descriptions: bool,
    pub include_schemas: bool,
    pub activity_entries_limit: usize,
    /// Compression strategy when context exceeds limits
    pub compression_strategy: CompressionStrategy,
    /// Hard cap on messages (safety limit, regardless of tokens)
    pub max_messages_cap: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            default_limits: ModelContextLimits::large(),
            model_overrides: HashMap::new(),
            include_descriptions: true,
            include_schemas: false,
            activity_entries_limit: 15,
            compression_strategy: CompressionStrategy::default(),
            max_messages_cap: 500,
        }
    }
}

impl ContextConfig {
    pub fn limits_for_model(&self, model_id: Option<&str>) -> &ModelContextLimits {
        model_id
            .and_then(|id| self.model_overrides.get(id))
            .unwrap_or(&self.default_limits)
    }
}

/// Rendered block for context inclusion
#[derive(Debug, Clone)]
pub struct RenderedBlock {
    pub label: String,
    pub block_type: BlockType,
    pub content: String,
    pub description: Option<String>,
    pub estimated_tokens: usize,
}

/// Tool description for system prompt
#[derive(Debug, Clone)]
pub struct ToolDescription {
    pub name: String,
    pub description: String,
    pub parameters: Vec<ParameterDescription>,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ParameterDescription {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// Hint for Anthropic prompt caching
#[derive(Debug, Clone)]
pub struct CachePoint {
    /// Label for debugging
    pub label: String,
    /// Position in the prompt where cache should be placed
    pub position: CachePosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePosition {
    /// After system prompt
    AfterSystem,
    /// After memory blocks
    AfterMemory,
    /// After tool definitions
    AfterTools,
    /// Custom position (message index from start)
    MessageIndex(usize),
}

/// Tool definition for model requests
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

/// Metadata about how context was built
#[derive(Debug, Clone)]
pub struct ContextMetadata {
    /// Estimated token count
    pub estimated_tokens: usize,
    /// Number of messages included
    pub message_count: usize,
    /// Number of messages archived/compressed
    pub messages_archived: usize,
    /// Whether compression was applied
    pub compression_applied: bool,
    /// Memory blocks included
    pub blocks_included: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_context_config() {
        let config = ContextConfig::default();

        assert_eq!(config.default_limits.max_tokens, 200_000);
        assert_eq!(config.default_limits.memory_tokens, 12_000);
        assert_eq!(config.default_limits.history_tokens, 80_000);
        assert_eq!(config.default_limits.reserved_response_tokens, 8_000);
        assert!(config.include_descriptions);
        assert!(!config.include_schemas);
        assert_eq!(config.activity_entries_limit, 15);
        assert_eq!(config.max_messages_cap, 500);
        match config.compression_strategy {
            CompressionStrategy::Truncate { keep_recent } => assert_eq!(keep_recent, 100),
            _ => panic!("Expected default to be Truncate strategy"),
        }
    }

    #[test]
    fn test_model_limits() {
        let large = ModelContextLimits::large();
        assert_eq!(large.max_tokens, 200_000);
        assert_eq!(large.memory_tokens, 12_000);

        let small = ModelContextLimits::small();
        assert_eq!(small.max_tokens, 200_000);
        assert_eq!(small.memory_tokens, 6_000);
    }

    #[test]
    fn test_limits_for_model() {
        let mut config = ContextConfig::default();

        // Test default limits when no model specified
        let limits = config.limits_for_model(None);
        assert_eq!(limits.max_tokens, 200_000);

        // Test default limits when model not in overrides
        let limits = config.limits_for_model(Some("unknown-model"));
        assert_eq!(limits.max_tokens, 200_000);

        // Test model-specific override
        config
            .model_overrides
            .insert("small-model".to_string(), ModelContextLimits::small());
        let limits = config.limits_for_model(Some("small-model"));
        assert_eq!(limits.memory_tokens, 6_000);
    }
}
