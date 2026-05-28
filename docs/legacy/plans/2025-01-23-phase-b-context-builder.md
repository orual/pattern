# Phase B: ContextBuilder Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create a new ContextBuilder in context_v2 that reads from MemoryStore, MessageStore, and ToolRegistry to produce a `Request` ready for model calls.

**Architecture:** Read-only builder pattern. Receives references to stores and registry, builds system prompt from memory blocks, handles message history with compression, produces model-ready `Request` directly (no intermediate MemoryContext).

**Tech Stack:** Rust async, pattern_db (SQLite), existing compression.rs logic

**Dependencies:** Phase A complete (SearchOptions, MessageStore, context_v2 types)

**Output Type:** `crate::message::Request` which is:
```rust
pub struct Request {
    pub system: Option<Vec<String>>,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<genai::chat::Tool>>,
}
```

---

## Task B1: Create ContextBuilder Core

**Files:**
- Create: `crates/pattern_core/src/context_v2/builder.rs`
- Modify: `crates/pattern_core/src/context_v2/mod.rs`

### Step 1: Write failing test for ContextBuilder

```rust
// In crates/pattern_core/src/context_v2/builder.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_context_builder_builds_request() {
        // Setup: Create mock MemoryStore with a core block
        let memory = MockMemoryStore::new();
        memory.add_block("persona", "You are a helpful assistant.", BlockType::Core);

        let config = ContextConfig::default();
        let builder = ContextBuilder::new(&memory, &config);

        let request = builder.build().await.unwrap();

        // Request.system is Option<Vec<String>>
        let system = request.system.expect("should have system prompt");
        assert!(system.iter().any(|s| s.contains("You are a helpful assistant")));
    }
}
```

### Step 2: Run test to verify it fails

Run: `cargo test -p pattern-core context_v2::builder::tests::test_context_builder_builds_request`
Expected: FAIL - module doesn't exist

### Step 3: Create basic ContextBuilder struct

```rust
// crates/pattern_core/src/context_v2/builder.rs

//! ContextBuilder: Assembles model requests from stores
//!
//! Read-only access to MemoryStore, MessageStore, and ToolRegistry.
//! Produces Request ready for model calls directly.

use crate::memory_v2::{MemoryStore, BlockType};
use crate::messages::MessageStore;
use crate::message::{Message, Request};
use crate::tool::ToolRegistry;
use super::types::ContextConfig;
use crate::error::CoreError;

/// Builder for assembling model requests from stores
pub struct ContextBuilder<'a> {
    memory: &'a dyn MemoryStore,
    messages: Option<&'a MessageStore>,
    tools: Option<&'a ToolRegistry>,
    config: &'a ContextConfig,
    agent_id: String,
}

impl<'a> ContextBuilder<'a> {
    /// Create a new ContextBuilder
    pub fn new(
        memory: &'a dyn MemoryStore,
        config: &'a ContextConfig,
    ) -> Self {
        Self {
            memory,
            messages: None,
            tools: None,
            config,
            agent_id: String::new(),
        }
    }

    /// Set the agent ID for scoped operations
    pub fn for_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = agent_id.into();
        self
    }

    /// Add message store for history
    pub fn with_messages(mut self, messages: &'a MessageStore) -> Self {
        self.messages = Some(messages);
        self
    }

    /// Add tool registry
    pub fn with_tools(mut self, tools: &'a ToolRegistry) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Build the complete Request
    pub async fn build(self) -> Result<Request, CoreError> {
        // Build system prompt from memory blocks
        let system_prompt = self.build_system_prompt().await?;

        // Get messages if store provided
        let messages = if let Some(msg_store) = self.messages {
            let limits = self.config.limits_for_model(None);
            // Rough estimate: 4 chars per token, get enough for history
            msg_store.get_recent(limits.history_tokens / 4).await?
        } else {
            Vec::new()
        };

        // Get tools in genai format if registry provided
        let tools = self.build_genai_tools();

        Ok(Request {
            system: Some(vec![system_prompt]),
            messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
        })
    }

    /// Build system prompt from memory blocks
    async fn build_system_prompt(&self) -> Result<String, CoreError> {
        let mut sections = Vec::new();

        // Get core memory blocks
        let core_blocks = self.memory
            .list_blocks_by_type(&self.agent_id, BlockType::Core)
            .await
            .map_err(CoreError::MemoryError)?;

        for block_meta in core_blocks {
            if let Some(content) = self.memory
                .get_rendered_content(&self.agent_id, &block_meta.label)
                .await
                .map_err(CoreError::MemoryError)?
            {
                let section = if self.config.include_descriptions {
                    format!("<{}>\n{}: {}\n{}\n</{}>",
                        block_meta.label,
                        block_meta.label,
                        block_meta.description,
                        content,
                        block_meta.label
                    )
                } else {
                    format!("<{}>\n{}\n</{}>", block_meta.label, content, block_meta.label)
                };
                sections.push(section);
            }
        }

        // Get working memory blocks
        let working_blocks = self.memory
            .list_blocks_by_type(&self.agent_id, BlockType::Working)
            .await
            .map_err(CoreError::MemoryError)?;

        for block_meta in working_blocks {
            if let Some(content) = self.memory
                .get_rendered_content(&self.agent_id, &block_meta.label)
                .await
                .map_err(CoreError::MemoryError)?
            {
                sections.push(format!("<{}>\n{}\n</{}>",
                    block_meta.label, content, block_meta.label));
            }
        }

        Ok(sections.join("\n\n"))
    }

    /// Build tools in genai format from registry
    fn build_genai_tools(&self) -> Vec<genai::chat::Tool> {
        let Some(registry) = self.tools else {
            return Vec::new();
        };

        registry.list_tools()
            .iter()
            .filter_map(|name| {
                registry.get(name).map(|tool| tool.to_genai_tool())
            })
            .collect()
    }
}
```

### Step 4: Update mod.rs exports

```rust
// Add to crates/pattern_core/src/context_v2/mod.rs

pub mod builder;

pub use builder::ContextBuilder;
```

### Step 5: Run test to verify it passes

Run: `cargo test -p pattern-core context_v2::builder::tests`
Expected: PASS (or update test based on actual implementation)

### Step 6: Commit

```bash
git add crates/pattern_core/src/context_v2/builder.rs crates/pattern_core/src/context_v2/mod.rs
git commit -m "feat(context_v2): add ContextBuilder core"
```

---

## Task B2: Add Model-Specific Optimizations

**Files:**
- Modify: `crates/pattern_core/src/context_v2/builder.rs`
- Modify: `crates/pattern_core/src/context_v2/types.rs`

### Step 1: Write failing test for model-aware building

```rust
#[tokio::test]
async fn test_gemini_message_adjustment() {
    let memory = MockMemoryStore::new();
    let mut messages = MockMessageStore::new();
    // Add assistant message first (invalid for Gemini)
    messages.add(Message::assistant("Hello"));
    messages.add(Message::user("Hi"));

    let config = ContextConfig::default();
    let builder = ContextBuilder::new(&memory, &config)
        .for_agent("test")
        .with_messages(&messages)
        .with_model_info(ModelInfo {
            provider: "gemini".into(),
            model_id: "gemini-1.5-pro".into(),
            ..Default::default()
        });

    let request = builder.build().await.unwrap();

    // Gemini requires first message to be user - should have been adjusted
    assert_eq!(request.messages.first().unwrap().role, ChatRole::User);
}
```

### Step 2: Add ModelInfo to builder

```rust
impl<'a> ContextBuilder<'a> {
    /// Set model info for provider-specific optimizations
    pub fn with_model_info(mut self, model_info: ModelInfo) -> Self {
        self.model_info = Some(model_info);
        self
    }

    /// Build cache control points based on provider
    fn build_cache_control(&self, system_prompt_len: usize) -> Vec<CachePoint> {
        let Some(model_info) = &self.model_info else {
            return Vec::new();
        };

        match model_info.provider.as_str() {
            "anthropic" => {
                vec![
                    CachePoint {
                        label: "system_prompt".into(),
                        position: CachePosition::AfterSystem,
                    },
                    CachePoint {
                        label: "memory_blocks".into(),
                        position: CachePosition::AfterMemory,
                    },
                ]
            }
            _ => Vec::new(),
        }
    }
}
```

### Step 3: Add ModelInfo struct

```rust
// In types.rs

#[derive(Debug, Clone, Default)]
pub struct ModelInfo {
    pub provider: String,
    pub model_id: String,
    pub max_tokens: Option<usize>,
    pub supports_caching: bool,
}
```

### Step 4: Add Gemini message validation

```rust
impl<'a> ContextBuilder<'a> {
    /// Validate and adjust messages for Gemini
    fn adjust_for_gemini(&self, messages: &mut Vec<Message>) {
        // Gemini requires:
        // 1. First message must be user role
        // 2. No empty content
        // 3. Alternating user/assistant pattern (with some flexibility)

        // Remove empty messages
        messages.retain(|m| !m.content.is_empty());

        // Ensure first message is user (prepend system as user if needed)
        if let Some(first) = messages.first() {
            if first.role != ChatRole::User {
                // Insert synthetic user message
                messages.insert(0, Message::user("[Conversation start]"));
            }
        }
    }
}
```

### Step 5: Commit

```bash
git add crates/pattern_core/src/context_v2/builder.rs crates/pattern_core/src/context_v2/types.rs
git commit -m "feat(context_v2): add model-specific optimizations (cache, Gemini)"
```

---

## Task B3: Compression Integration

**Files:**
- Modify: `crates/pattern_core/src/context_v2/builder.rs`
- Create: `crates/pattern_core/src/context_v2/compression.rs` (adapter)

### Step 1: Write failing test for compression

```rust
#[tokio::test]
async fn test_compression_when_over_token_limit() {
    let memory = MockMemoryStore::new();
    // Create messages with known token sizes (~100 tokens each = ~400 chars)
    let messages = MockMessageStore::with_messages_of_size(50, 400);

    let mut config = ContextConfig::default();
    // Set history_tokens low to trigger compression (50 msgs * 100 tokens = 5000)
    config.default_limits.history_tokens = 2000; // Should keep ~20 messages

    let builder = ContextBuilder::new(&memory, &config)
        .for_agent("test")
        .with_messages(&messages);

    let request = builder.build().await.unwrap();

    // Should have compressed based on token limit
    // ~2000 tokens / ~100 tokens per msg = ~20 messages
    assert!(request.messages.len() < 50);
    assert!(request.messages.len() >= 15); // Rough range
}

#[tokio::test]
async fn test_compression_respects_safety_cap() {
    let memory = MockMemoryStore::new();
    let messages = MockMessageStore::with_messages(1000);

    let mut config = ContextConfig::default();
    config.max_messages_cap = 200; // Safety cap
    config.default_limits.history_tokens = 1_000_000; // High token limit

    let builder = ContextBuilder::new(&memory, &config)
        .for_agent("test")
        .with_messages(&messages);

    let request = builder.build().await.unwrap();

    // Should respect safety cap even if tokens allow more
    assert!(request.messages.len() <= 200);
}
```

### Step 2: Add compression fields to ContextConfig

```rust
// Add to existing ContextConfig in types.rs

#[derive(Debug, Clone)]
pub struct ContextConfig {
    // Existing fields:
    pub default_limits: ModelContextLimits,
    pub model_overrides: HashMap<String, ModelContextLimits>,
    pub include_descriptions: bool,
    pub include_schemas: bool,
    pub activity_entries_limit: usize,

    // New compression fields:
    /// Compression strategy when context exceeds limits
    pub compression_strategy: CompressionStrategy,
    /// Hard cap on messages (safety limit, regardless of tokens)
    pub max_messages_cap: usize,
}

// Update Default impl to include new fields
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            default_limits: ModelContextLimits::large(),
            model_overrides: HashMap::new(),
            include_descriptions: true,
            include_schemas: false,
            activity_entries_limit: 15,
            // New defaults:
            compression_strategy: CompressionStrategy::Truncate { keep_recent: 100 },
            max_messages_cap: 500, // Safety cap
        }
    }
}
```

**Note:** Compression is primarily driven by `ModelContextLimits::history_tokens` (model-specific), with `max_messages_cap` as a global safety limit.

### Step 3: Port compression logic to builder

```rust
impl<'a> ContextBuilder<'a> {
    /// Build the complete Request (updated with compression)
    pub async fn build(self) -> Result<Request, CoreError> {
        // Build system prompt from memory blocks
        let system_prompt = self.build_system_prompt().await?;

        // Get messages if store provided
        let mut messages = if let Some(msg_store) = self.messages {
            // Fetch up to safety cap
            msg_store.get_all(self.config.max_messages_cap).await?
        } else {
            Vec::new()
        };

        // Get model-specific limits
        let limits = self.config.limits_for_model(
            self.model_info.as_ref().map(|m| m.model_id.as_str())
        );

        // Apply compression based on token limits
        messages = self.compress_to_token_limit(messages, limits.history_tokens);

        // Get tools in genai format if registry provided
        let tools = self.build_genai_tools();

        Ok(Request {
            system: Some(vec![system_prompt]),
            messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
        })
    }

    /// Compress messages to fit within token limit
    fn compress_to_token_limit(
        &self,
        mut messages: Vec<Message>,
        max_tokens: usize,
    ) -> Vec<Message> {
        // First check global safety cap
        if messages.len() > self.config.max_messages_cap {
            let start = messages.len() - self.config.max_messages_cap;
            messages = messages.split_off(start);
        }

        // Estimate tokens and compress if needed
        let mut total_tokens = self.estimate_message_tokens(&messages);

        if total_tokens <= max_tokens {
            return messages;
        }

        // Apply compression strategy
        match &self.config.compression_strategy {
            CompressionStrategy::Truncate { keep_recent } => {
                // Remove oldest messages until under limit, keeping at least keep_recent
                while total_tokens > max_tokens && messages.len() > *keep_recent {
                    if let Some(removed) = messages.first() {
                        total_tokens -= self.estimate_single_message_tokens(removed);
                    }
                    messages.remove(0);
                }
            }
            CompressionStrategy::RecursiveSummarization { .. } => {
                // TODO: Integrate with ModelProvider for summarization
                // For now, fall back to truncation
                while total_tokens > max_tokens && messages.len() > 10 {
                    if let Some(removed) = messages.first() {
                        total_tokens -= self.estimate_single_message_tokens(removed);
                    }
                    messages.remove(0);
                }
            }
            _ => {
                // Other strategies: fall back to truncation
                while total_tokens > max_tokens && messages.len() > 10 {
                    if let Some(removed) = messages.first() {
                        total_tokens -= self.estimate_single_message_tokens(removed);
                    }
                    messages.remove(0);
                }
            }
        }

        messages
    }

    /// Estimate tokens for all messages
    fn estimate_message_tokens(&self, messages: &[Message]) -> usize {
        messages.iter().map(|m| self.estimate_single_message_tokens(m)).sum()
    }

    /// Estimate tokens for a single message (rough: ~4 chars per token)
    fn estimate_single_message_tokens(&self, message: &Message) -> usize {
        message.content.to_string().len() / 4 + 4 // +4 for role overhead
    }
}
```

### Step 4: Add ModelProvider for summarization (optional extension)

```rust
impl<'a> ContextBuilder<'a> {
    /// Add model provider for summarization-based compression
    pub fn with_model_provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.model_provider = Some(provider);
        self
    }

    /// Summarize messages using model
    async fn summarize_messages(
        &self,
        messages: &[Message],
        prompt: &str,
    ) -> Result<String, CoreError> {
        let Some(provider) = &self.model_provider else {
            return Err(CoreError::ConfigurationError {
                field: "model_provider".into(),
                message: "Model provider required for summarization".into(),
            });
        };

        // Build summarization request
        // ... implementation using provider.complete() ...

        todo!("Implement summarization request")
    }
}
```

### Step 5: Commit

```bash
git add crates/pattern_core/src/context_v2/builder.rs crates/pattern_core/src/context_v2/types.rs
git commit -m "feat(context_v2): add compression integration to ContextBuilder"
```

---

## Success Criteria

- [ ] `cargo check -p pattern-core` passes
- [ ] `cargo test -p pattern-core context_v2` passes
- [ ] ContextBuilder reads from MemoryStore trait
- [ ] ContextBuilder reads from MessageStore
- [ ] ContextBuilder reads from ToolRegistry
- [ ] ContextBuilder returns `Request` directly (not intermediate type)
- [ ] System prompt includes memory blocks
- [ ] Gemini message validation applied (first message is user)
- [ ] Compression driven by `history_tokens` limit (model-specific)
- [ ] Safety cap `max_messages_cap` respected

---

## Notes

- ContextBuilder is **read-only** - it doesn't mutate stores
- ContextBuilder returns `Request` directly - no intermediate MemoryContext needed
- Compression is **in-memory** for context building - actual archival happens elsewhere
- Compression is **token-driven** using `ModelContextLimits::history_tokens`
- Token estimation is rough (4 chars/token) - can be refined later or use model's tokenizer
