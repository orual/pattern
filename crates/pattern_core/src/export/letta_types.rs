//! Serde types for Letta Agent File (.af) JSON format.
//!
//! These types mirror the Letta Python schema from `letta/schemas/agent_file.py`.
//! The .af format is plain JSON containing all state needed to recreate an agent.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Deserialize null as empty Vec (Letta uses null instead of [] in many places)
fn null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

/// Root container for agent file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFileSchema {
    /// List of agents in the file
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub agents: Vec<AgentSchema>,

    /// Groups containing multiple agents
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub groups: Vec<GroupSchema>,

    /// Memory blocks (shared across agents)
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub blocks: Vec<BlockSchema>,

    /// File metadata
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub files: Vec<FileSchema>,

    /// Data sources (folders)
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub sources: Vec<SourceSchema>,

    /// Tool definitions with source code
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub tools: Vec<ToolSchema>,

    /// MCP server configurations
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub mcp_servers: Vec<McpServerSchema>,

    /// Additional metadata
    #[serde(default)]
    pub metadata: Option<Value>,

    /// When this file was created
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

/// Agent configuration and state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSchema {
    /// Unique identifier
    pub id: String,

    /// Agent name
    #[serde(default)]
    pub name: Option<String>,

    /// Agent type (e.g., "letta_v1_agent"). None = newest version.
    #[serde(default)]
    pub agent_type: Option<String>,

    /// System prompt / base instructions
    #[serde(default)]
    pub system: Option<String>,

    /// Agent description
    #[serde(default)]
    pub description: Option<String>,

    /// Additional metadata
    #[serde(default)]
    pub metadata: Option<Value>,

    /// Memory block definitions (inline)
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub memory_blocks: Vec<CreateBlockSchema>,

    /// Tool IDs this agent can use
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub tool_ids: Vec<String>,

    /// Legacy tool names
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub tools: Vec<String>,

    /// Tool execution rules
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub tool_rules: Vec<LettaToolRule>,

    /// Block IDs attached to this agent
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub block_ids: Vec<String>,

    /// Include base tools (memory, search, etc.)
    #[serde(default)]
    pub include_base_tools: Option<bool>,

    /// Include multi-agent tools
    #[serde(default)]
    pub include_multi_agent_tools: Option<bool>,

    /// Model in "provider/model-name" format
    #[serde(default)]
    pub model: Option<String>,

    /// Embedding model in "provider/model-name" format
    #[serde(default)]
    pub embedding: Option<String>,

    /// LLM configuration (deprecated but still used)
    #[serde(default)]
    pub llm_config: Option<LlmConfig>,

    /// Embedding configuration (deprecated but still used)
    #[serde(default)]
    pub embedding_config: Option<EmbeddingConfig>,

    /// Message IDs currently in context
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub in_context_message_ids: Vec<String>,

    /// Full message history
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub messages: Vec<MessageSchema>,

    /// File associations
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub files_agents: Vec<FileAgentSchema>,

    /// Group memberships
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub group_ids: Vec<String>,
}

impl AgentSchema {
    /// Returns whether base tools should be included (defaults to true)
    pub fn include_base_tools(&self) -> bool {
        self.include_base_tools.unwrap_or(true)
    }
}

/// Message in conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSchema {
    /// Unique identifier
    pub id: String,

    /// Message role: "system", "user", "assistant", "tool"
    #[serde(default)]
    pub role: Option<String>,

    /// Message content (text or structured)
    #[serde(default)]
    pub content: Option<Value>,

    /// Text content (alternative to structured content)
    #[serde(default)]
    pub text: Option<String>,

    /// Model that generated this message
    #[serde(default)]
    pub model: Option<String>,

    /// Agent that owns this message
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Tool calls made in this message
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub tool_calls: Vec<ToolCallSchema>,

    /// Tool call ID this message responds to
    #[serde(default)]
    pub tool_call_id: Option<String>,

    /// Tool return values
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub tool_returns: Vec<ToolReturnSchema>,

    /// When this message was created
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,

    /// Whether this message is in the current context window
    #[serde(default)]
    pub in_context: Option<bool>,
}

/// Tool call within a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallSchema {
    /// Tool call ID
    #[serde(default)]
    pub id: Option<String>,

    /// Tool function details
    #[serde(default)]
    pub function: Option<ToolCallFunction>,

    /// Type (usually "function")
    #[serde(default)]
    pub r#type: Option<String>,
}

/// Tool call function details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    /// Function name
    #[serde(default)]
    pub name: Option<String>,

    /// Arguments as JSON string
    #[serde(default)]
    pub arguments: Option<String>,
}

/// Tool return value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolReturnSchema {
    /// Tool call ID this responds to
    #[serde(default)]
    pub tool_call_id: Option<String>,

    /// Return value
    #[serde(default)]
    pub content: Option<Value>,

    /// Status
    #[serde(default)]
    pub status: Option<String>,
}

// =============================================================================
// Tool Rules
// =============================================================================

/// Letta tool rule - controls tool execution behavior.
/// Uses serde's internally tagged representation to handle polymorphic JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LettaToolRule {
    /// Tool that ends the agent turn (like send_message)
    #[serde(rename = "TerminalToolRule")]
    Terminal {
        #[serde(default)]
        tool_name: Option<String>,
    },

    /// Tool that must be called first in a turn
    #[serde(rename = "InitToolRule")]
    Init {
        #[serde(default)]
        tool_name: Option<String>,
    },

    /// Tool that must be followed by specific other tools
    #[serde(rename = "ChildToolRule")]
    Child {
        #[serde(default)]
        tool_name: Option<String>,
        #[serde(default, deserialize_with = "null_as_empty_vec")]
        children: Vec<String>,
    },

    /// Tool that requires specific tools to have been called before it
    #[serde(rename = "ParentToolRule")]
    Parent {
        #[serde(default)]
        tool_name: Option<String>,
        #[serde(default, deserialize_with = "null_as_empty_vec")]
        parents: Vec<String>,
    },

    /// Tool that continues the agent loop (opposite of terminal)
    #[serde(rename = "ContinueToolRule")]
    Continue {
        #[serde(default)]
        tool_name: Option<String>,
    },

    /// Limit how many times a tool can be called per step
    #[serde(rename = "MaxCountPerStepToolRule")]
    MaxCountPerStep {
        #[serde(default)]
        tool_name: Option<String>,
        #[serde(default)]
        max_count: Option<i64>,
    },

    /// Conditional tool execution based on state
    #[serde(rename = "ConditionalToolRule")]
    Conditional {
        #[serde(default)]
        tool_name: Option<String>,
        #[serde(default)]
        condition: Option<Value>,
    },
}

/// Memory block definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSchema {
    /// Unique identifier
    pub id: String,

    /// Block label (e.g., "persona", "human")
    #[serde(default)]
    pub label: Option<String>,

    /// Block content
    #[serde(default)]
    pub value: Option<String>,

    /// Character limit
    #[serde(default)]
    pub limit: Option<i64>,

    /// Whether this is a template
    #[serde(default)]
    pub is_template: Option<bool>,

    /// Template name if applicable
    #[serde(default)]
    pub template_name: Option<String>,

    /// Read-only flag
    #[serde(default)]
    pub read_only: Option<bool>,

    /// Block description
    #[serde(default)]
    pub description: Option<String>,

    /// Additional metadata
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// Inline block creation (used in agent.memory_blocks).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBlockSchema {
    /// Block label
    #[serde(default)]
    pub label: Option<String>,

    /// Block content
    #[serde(default)]
    pub value: Option<String>,

    /// Character limit
    #[serde(default)]
    pub limit: Option<i64>,

    /// Template name
    #[serde(default)]
    pub template_name: Option<String>,

    /// Read-only flag
    #[serde(default)]
    pub read_only: Option<bool>,

    /// Block description
    #[serde(default)]
    pub description: Option<String>,

    /// Additional metadata
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// Group containing multiple agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupSchema {
    /// Unique identifier
    pub id: String,

    /// Agent IDs in this group
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub agent_ids: Vec<String>,

    /// Group description
    #[serde(default)]
    pub description: Option<String>,

    /// Manager configuration
    #[serde(default)]
    pub manager_config: Option<Value>,

    /// Project ID
    #[serde(default)]
    pub project_id: Option<String>,

    /// Shared block IDs
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub shared_block_ids: Vec<String>,
}

/// Tool definition with source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Unique identifier
    pub id: String,

    /// Tool/function name
    #[serde(default)]
    pub name: Option<String>,

    /// Tool type category
    #[serde(default)]
    pub tool_type: Option<String>,

    /// Description
    #[serde(default)]
    pub description: Option<String>,

    /// Python source code
    #[serde(default)]
    pub source_code: Option<String>,

    /// Source language
    #[serde(default)]
    pub source_type: Option<String>,

    /// JSON schema for the function
    #[serde(default)]
    pub json_schema: Option<Value>,

    /// Argument-specific schema
    #[serde(default)]
    pub args_json_schema: Option<Value>,

    /// Tags
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub tags: Vec<String>,

    /// Return character limit
    #[serde(default)]
    pub return_char_limit: Option<i64>,

    /// Requires approval to execute
    #[serde(default)]
    pub default_requires_approval: Option<bool>,
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSchema {
    /// Unique identifier
    pub id: String,

    /// Server type
    #[serde(default)]
    pub server_type: Option<String>,

    /// Server name
    #[serde(default)]
    pub server_name: Option<String>,

    /// Server URL (for HTTP/SSE)
    #[serde(default)]
    pub server_url: Option<String>,

    /// Stdio configuration (for subprocess)
    #[serde(default)]
    pub stdio_config: Option<Value>,

    /// Additional metadata
    #[serde(default)]
    pub metadata_: Option<Value>,
}

/// File metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSchema {
    /// Unique identifier
    pub id: String,

    /// Original filename
    #[serde(default)]
    pub file_name: Option<String>,

    /// File size in bytes
    #[serde(default)]
    pub file_size: Option<i64>,

    /// MIME type
    #[serde(default)]
    pub file_type: Option<String>,

    /// File content (if embedded)
    #[serde(default)]
    pub content: Option<String>,
}

/// File-agent association.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAgentSchema {
    /// Unique identifier
    pub id: String,

    /// Agent ID
    #[serde(default)]
    pub agent_id: Option<String>,

    /// File ID
    #[serde(default)]
    pub file_id: Option<String>,

    /// Source ID
    #[serde(default)]
    pub source_id: Option<String>,

    /// Filename
    #[serde(default)]
    pub file_name: Option<String>,

    /// Whether file is currently open
    #[serde(default)]
    pub is_open: Option<bool>,

    /// Visible content portion
    #[serde(default)]
    pub visible_content: Option<String>,
}

/// Data source (folder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSchema {
    /// Unique identifier
    pub id: String,

    /// Source name
    #[serde(default)]
    pub name: Option<String>,

    /// Description
    #[serde(default)]
    pub description: Option<String>,

    /// Processing instructions
    #[serde(default)]
    pub instructions: Option<String>,

    /// Additional metadata
    #[serde(default)]
    pub metadata: Option<Value>,

    /// Embedding configuration
    #[serde(default)]
    pub embedding_config: Option<EmbeddingConfig>,
}

/// LLM configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Model name
    #[serde(default)]
    pub model: Option<String>,

    /// Model endpoint type
    #[serde(default)]
    pub model_endpoint_type: Option<String>,

    /// Model endpoint URL
    #[serde(default)]
    pub model_endpoint: Option<String>,

    /// Context window size
    #[serde(default)]
    pub context_window: Option<i64>,

    /// Temperature
    #[serde(default)]
    pub temperature: Option<f64>,

    /// Max tokens to generate
    #[serde(default)]
    pub max_tokens: Option<i64>,
}

/// Embedding configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Embedding model name
    #[serde(default)]
    pub embedding_model: Option<String>,

    /// Embedding endpoint type
    #[serde(default)]
    pub embedding_endpoint_type: Option<String>,

    /// Embedding endpoint URL
    #[serde(default)]
    pub embedding_endpoint: Option<String>,

    /// Embedding dimension
    #[serde(default)]
    pub embedding_dim: Option<i64>,

    /// Chunk size for splitting
    #[serde(default)]
    pub embedding_chunk_size: Option<i64>,
}

// =============================================================================
// Tool Name Mapping
// =============================================================================

/// Known Letta tool names and their Pattern equivalents.
pub struct ToolMapping;

impl ToolMapping {
    /// Map a Letta tool name to Pattern tool name(s).
    /// Returns None if the tool should be dropped (no equivalent).
    pub fn map_tool(letta_name: &str) -> Option<Vec<&'static str>> {
        match letta_name {
            // Memory tools -> context
            "memory_insert" | "memory_replace" | "memory_rethink" => Some(vec!["context"]),
            "memory_finish_edits" => None, // No equivalent

            // Search tools
            "conversation_search" => Some(vec!["search"]),
            "archival_memory_search" => Some(vec!["recall", "search"]),
            "archival_memory_insert" => Some(vec!["recall"]),

            // Communication
            "send_message" => Some(vec!["send_message"]),

            // Web tools
            "web_search" | "fetch_webpage" => Some(vec!["web"]),

            // File tools
            "open_file" | "grep_file" | "search_file" => Some(vec!["file"]),

            // Code execution - no equivalent
            "run_code" => None,

            // Unknown tool - pass through name as-is (might match a Pattern tool)
            _ => Some(vec![]),
        }
    }

    /// Get the default tools that should always be included.
    pub fn default_tools() -> Vec<&'static str> {
        vec![
            "context",
            "recall",
            "search",
            "send_message",
            "file",
            "source",
        ]
    }

    /// Build the final enabled_tools list from Letta agent config.
    pub fn build_enabled_tools(agent: &AgentSchema, all_tools: &[ToolSchema]) -> Vec<String> {
        use std::collections::HashSet;

        let mut tools: HashSet<String> = HashSet::new();

        // Start with defaults
        for t in Self::default_tools() {
            tools.insert(t.to_string());
        }

        // If agent_type is None (new-style), ensure send_message is present
        if agent.agent_type.is_none() {
            tools.insert("send_message".to_string());
        }

        // Map tool_ids to Pattern equivalents
        for tool_id in &agent.tool_ids {
            // Find the tool by ID
            if let Some(tool) = all_tools.iter().find(|t| &t.id == tool_id) {
                if let Some(ref name) = tool.name {
                    if let Some(mapped) = Self::map_tool(name) {
                        for m in mapped {
                            tools.insert(m.to_string());
                        }
                    }
                }
            }
        }

        // Map legacy tool names
        for tool_name in &agent.tools {
            if let Some(mapped) = Self::map_tool(tool_name) {
                for m in mapped {
                    tools.insert(m.to_string());
                }
            }
        }

        // If include_base_tools is true (or None, defaulting to true), add core tools
        if agent.include_base_tools() {
            tools.insert("context".to_string());
            tools.insert("recall".to_string());
            tools.insert("search".to_string());
        }

        // If there are file associations, ensure file tools
        if !agent.files_agents.is_empty() {
            tools.insert("file".to_string());
            tools.insert("source".to_string());
        }

        tools.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_mapping() {
        assert_eq!(
            ToolMapping::map_tool("memory_insert"),
            Some(vec!["context"])
        );
        assert_eq!(
            ToolMapping::map_tool("archival_memory_search"),
            Some(vec!["recall", "search"])
        );
        assert_eq!(ToolMapping::map_tool("run_code"), None);
        assert_eq!(ToolMapping::map_tool("unknown_tool"), Some(vec![]));
    }

    #[test]
    fn test_parse_minimal_agent_file() {
        let json = r#"{
            "agents": [{
                "id": "agent-123",
                "name": "Test Agent",
                "system": "You are a helpful assistant.",
                "model": "anthropic/claude-sonnet-4-5-20250929"
            }],
            "blocks": [],
            "tools": []
        }"#;

        let parsed: AgentFileSchema = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.agents.len(), 1);
        assert_eq!(parsed.agents[0].id, "agent-123");
        assert_eq!(
            parsed.agents[0].model.as_deref(),
            Some("anthropic/claude-sonnet-4-5-20250929")
        );
    }

    #[test]
    fn test_parse_nulls_as_empty() {
        let json = r#"{
            "agents": [{
                "id": "agent-123",
                "tool_ids": null,
                "tools": null,
                "messages": null
            }],
            "blocks": null,
            "tools": null
        }"#;

        let parsed: AgentFileSchema = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.agents.len(), 1);
        assert!(parsed.agents[0].tool_ids.is_empty());
        assert!(parsed.agents[0].tools.is_empty());
        assert!(parsed.agents[0].messages.is_empty());
        assert!(parsed.blocks.is_empty());
        assert!(parsed.tools.is_empty());
    }
}
