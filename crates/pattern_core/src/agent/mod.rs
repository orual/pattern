//! V2 Agent framework with slim trait design
//!
//! The AgentV2 trait is dramatically slimmer than the original Agent trait:
//! - Agent is just identity + process loop + state
//! - Runtime handles all "doing" (tool execution, message sending, storage)
//! - ContextBuilder handles all "reading" (memory, messages, tools → Request)
//! - Memory access is via tools, not direct trait methods

mod collect;
mod db_agent;
mod traits;

// Re-export tool_rules from tool module for backwards compatibility
pub mod tool_rules {
    pub use crate::tool::rules::*;
}

pub use collect::collect_response;
pub use db_agent::{DatabaseAgent, DatabaseAgentBuilder};
pub use traits::{Agent, AgentExt};

use crate::{
    AgentId, Result,
    message::{Message, MessageContent, Response, ToolCall, ToolResponse},
    tool::DynamicTool,
};

// Also re-export at agent module level for convenience
pub use crate::tool::rules::{
    ExecutionPhase, ToolExecution, ToolExecutionState, ToolRule, ToolRuleEngine, ToolRuleType,
    ToolRuleViolation,
};

use async_trait::async_trait;
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::str::FromStr;
use std::sync::Arc;
use tokio_stream::Stream;

/// Events emitted during message processing for real-time streaming
#[derive(Debug, Clone)]
pub enum ResponseEvent {
    /// Tool execution is starting
    ToolCallStarted {
        call_id: String,
        fn_name: String,
        args: serde_json::Value,
    },
    /// Tool execution completed (success or error)
    ToolCallCompleted {
        call_id: String,
        result: std::result::Result<String, String>,
    },
    /// Partial text chunk from the LLM response
    TextChunk {
        text: String,
        /// Whether this is a final chunk for this text block
        is_final: bool,
    },
    /// Partial reasoning/thinking content from the model
    ReasoningChunk {
        text: String,
        /// Whether this is a final chunk for this reasoning block
        is_final: bool,
    },
    /// Tool calls the agent is about to make
    ToolCalls { calls: Vec<ToolCall> },
    /// Tool responses received
    ToolResponses { responses: Vec<ToolResponse> },
    /// Processing complete with final metadata
    Complete {
        /// The ID of the incoming message that triggered this response
        message_id: crate::MessageId,
        /// Metadata about the complete response (usage, timing, etc)
        metadata: crate::message::ResponseMetadata,
    },
    /// An error occurred during processing
    Error { message: String, recoverable: bool },
}

/// Types of agents in the system
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub enum AgentType {
    /// Generic agent without specific personality
    Generic,

    /// ADHD-specific agent types
    #[cfg(feature = "nd")]
    /// Orchestrator agent - coordinates other agents and runs background checks
    Pattern,
    #[cfg(feature = "nd")]
    /// Task specialist - breaks down overwhelming tasks into atomic units
    Entropy,
    #[cfg(feature = "nd")]
    /// Time translator - converts between ADHD time and clock time
    Flux,
    #[cfg(feature = "nd")]
    /// Memory bank - external memory for context recovery and pattern finding
    Archive,
    #[cfg(feature = "nd")]
    /// Energy tracker - monitors energy patterns and protects flow states
    Momentum,
    #[cfg(feature = "nd")]
    /// Habit manager - manages routines and basic needs without nagging
    Anchor,

    /// Custom agent type
    Custom(String),
}

impl Serialize for AgentType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Generic => serializer.serialize_str("generic"),
            #[cfg(feature = "nd")]
            Self::Pattern => serializer.serialize_str("pattern"),
            #[cfg(feature = "nd")]
            Self::Entropy => serializer.serialize_str("entropy"),
            #[cfg(feature = "nd")]
            Self::Flux => serializer.serialize_str("flux"),
            #[cfg(feature = "nd")]
            Self::Archive => serializer.serialize_str("archive"),
            #[cfg(feature = "nd")]
            Self::Momentum => serializer.serialize_str("momentum"),
            #[cfg(feature = "nd")]
            Self::Anchor => serializer.serialize_str("anchor"),
            Self::Custom(name) => serializer.serialize_str(&format!("custom_{}", name)),
        }
    }
}

impl<'de> Deserialize<'de> for AgentType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        // Check if it starts with custom: prefix
        if let Some(name) = s.strip_prefix("custom_") {
            Ok(Self::Custom(name.to_string()))
        } else {
            Ok(Self::from_str(&s).unwrap_or_else(|_| Self::Custom(s)))
        }
    }
}

impl AgentType {
    /// Convert the agent type to its string representation
    ///
    /// For custom agents, returns the raw name without any prefix
    pub fn as_str(&self) -> &str {
        match self {
            Self::Generic => "generic",
            #[cfg(feature = "nd")]
            Self::Pattern => "pattern",
            #[cfg(feature = "nd")]
            Self::Entropy => "entropy",
            #[cfg(feature = "nd")]
            Self::Flux => "flux",
            #[cfg(feature = "nd")]
            Self::Archive => "archive",
            #[cfg(feature = "nd")]
            Self::Momentum => "momentum",
            #[cfg(feature = "nd")]
            Self::Anchor => "anchor",
            Self::Custom(name) => name, // Note: this returns the raw name without prefix
        }
    }
}

impl FromStr for AgentType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "generic" => Ok(Self::Generic),
            #[cfg(feature = "nd")]
            "pattern" => Ok(Self::Pattern),
            #[cfg(feature = "nd")]
            "entropy" => Ok(Self::Entropy),
            #[cfg(feature = "nd")]
            "flux" => Ok(Self::Flux),
            #[cfg(feature = "nd")]
            "archive" => Ok(Self::Archive),
            #[cfg(feature = "nd")]
            "momentum" => Ok(Self::Momentum),
            #[cfg(feature = "nd")]
            "anchor" => Ok(Self::Anchor),
            // Check for custom: prefix
            other if other.starts_with("custom:") => Ok(Self::Custom(
                other.strip_prefix("custom:").unwrap().to_string(),
            )),
            // For backward compatibility, also accept without prefix
            other => Ok(Self::Custom(other.to_string())),
        }
    }
}

/// Types of recoverable errors that agents can encounter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[non_exhaustive]
pub enum RecoverableErrorKind {
    /// Anthropic thinking mode message ordering error
    /// Note that anthropic often gives the index of the problematic message,
    /// TODO: Pass along and make use of this
    AnthropicThinkingOrder,

    /// Gemini empty contents error
    GeminiEmptyContents,

    /// Unpaired tool calls
    UnpairedToolCalls,

    /// Unpaired tool responses
    UnpairedToolResponses,

    /// Message compression failed
    MessageCompressionFailed,

    /// Context building failed
    ContextBuildFailed,

    /// Prompt exceeds token limit
    PromptTooLong,

    /// Model API error
    ModelApiError,

    /// Unknown error type
    Unknown,
}

impl RecoverableErrorKind {
    /// Parse an error message to determine the appropriate recovery kind
    pub fn from_error_str(error_str: &str) -> Self {
        let lower = error_str.to_lowercase();

        // Check for prompt too long errors
        if lower.contains("prompt is too long")
            || lower.contains("prompt") && lower.contains("too") && lower.contains("long")
            || (lower.contains("tokens") && lower.contains("maximum"))
            || lower.contains("context length exceeded")
        {
            return Self::PromptTooLong;
        }

        // Anthropic thinking mode errors
        if lower.contains("messages: roles must alternate")
            || lower.contains("messages does not match")
        {
            return Self::AnthropicThinkingOrder;
        }

        // Gemini empty contents errors
        if lower.contains("contents is not specified") || lower.contains("empty contents") {
            return Self::GeminiEmptyContents;
        }

        // Tool-related errors
        if (lower.contains("tool_use") && lower.contains("unpaired"))
            || (lower.contains("tool_use")
                && lower.contains("without")
                && lower.contains("tool_result"))
        {
            return Self::UnpairedToolCalls;
        }
        if (lower.contains("tool_result") && lower.contains("unpaired"))
            || (lower.contains("tool_result")
                && lower.contains("without")
                && lower.contains("tool_use"))
        {
            return Self::UnpairedToolResponses;
        }

        // Compression errors
        if lower.contains("compression") {
            return Self::MessageCompressionFailed;
        }

        // Context errors
        if lower.contains("context") || lower.contains("token") {
            return Self::ContextBuildFailed;
        }

        // Generic model API errors
        if lower.contains("api") || lower.contains("model") {
            return Self::ModelApiError;
        }

        Self::Unknown
    }

    /// Extract additional context from error messages (like Anthropic's index)
    pub fn extract_error_context(error_str: &str) -> Option<serde_json::Value> {
        // Try to extract index from Anthropic errors
        if error_str.contains("messages[") {
            // Look for pattern like "messages[5]" or "at index 5"
            let re = regex::Regex::new(r"messages\[(\d+)\]|at index (\d+)").ok()?;
            if let Some(captures) = re.captures(error_str) {
                let index = captures
                    .get(1)
                    .or_else(|| captures.get(2))
                    .and_then(|m| m.as_str().parse::<usize>().ok())?;
                return Some(serde_json::json!({
                    "problematic_index": index
                }));
            }
        }
        None
    }
}

/// The current state of an agent
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// Agent is ready to process messages
    Ready,

    /// Agent is currently processing a message
    Processing {
        /// Batches currently being processed
        active_batches: std::collections::HashSet<SnowflakePosition>,
    },

    /// Agent is in a cooldown period
    Cooldown { until: chrono::DateTime<Utc> },

    /// Agent is suspended
    Suspended,

    /// Agent has encountered an error
    Error {
        /// Type of error for recovery logic
        kind: RecoverableErrorKind,
        /// Error message for logging
        message: String,
    },
}

impl Default for AgentState {
    fn default() -> Self {
        Self::Ready
    }
}

impl FromStr for AgentState {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "ready" => Ok(Self::Ready),
            "processing" => Ok(Self::Processing {
                active_batches: std::collections::HashSet::new(),
            }),
            "suspended" => Ok(Self::Suspended),
            "error" => Ok(Self::Error {
                kind: RecoverableErrorKind::Unknown,
                message: String::new(),
            }),
            other => {
                // Try to parse as cooldown with timestamp
                if other.starts_with("cooldown:") {
                    let timestamp_str = &other[9..];
                    chrono::DateTime::parse_from_rfc3339(timestamp_str)
                        .map(|dt| Self::Cooldown {
                            until: dt.with_timezone(&Utc),
                        })
                        .map_err(|e| format!("Invalid cooldown timestamp: {}", e))
                } else {
                    Err(format!("Unknown agent state: {}", other))
                }
            }
        }
    }
}

/// Priority levels for agent actions and tasks
///
/// Used to determine the urgency and ordering of agent actions.
/// The variants are ordered from lowest to highest priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ActionPriority {
    /// Low priority - can be deferred or batched
    Low,
    /// Medium priority - normal operations
    Medium,
    /// High priority - should be handled soon
    High,
    /// Critical priority - requires immediate attention
    Critical,
}

use ferroid::{Base32SnowExt, SnowflakeGeneratorAsyncTokioExt, SnowflakeMastodonId};
use std::fmt;
use std::sync::OnceLock;

/// Wrapper type for Snowflake IDs with proper serde support
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnowflakePosition(pub SnowflakeMastodonId);

impl SnowflakePosition {
    /// Create a new snowflake position
    pub fn new(id: SnowflakeMastodonId) -> Self {
        Self(id)
    }
}

impl fmt::Display for SnowflakePosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Use the efficient base32 encoding via Display
        write!(f, "{}", self.0)
    }
}

impl FromStr for SnowflakePosition {
    type Err = String;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        // Try parsing as base32 first
        if let Ok(id) = SnowflakeMastodonId::decode(s) {
            return Ok(Self(id));
        }

        // Fall back to parsing as raw u64
        s.parse::<u64>()
            .map(|raw| Self(SnowflakeMastodonId::from_raw(raw)))
            .map_err(|e| format!("Failed to parse snowflake as base32 or u64: {}", e))
    }
}

impl Serialize for SnowflakePosition {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize as string using Display
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SnowflakePosition {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize from string and parse
        let s = String::deserialize(deserializer)?;
        s.parse::<Self>().map_err(serde::de::Error::custom)
    }
}

/// Type alias for the Snowflake generator we're using
type SnowflakeGen = ferroid::AtomicSnowflakeGenerator<SnowflakeMastodonId, ferroid::MonotonicClock>;

/// Global ID generator for message positions using Snowflake IDs
/// This provides distributed, monotonic IDs that work across processes
static MESSAGE_POSITION_GENERATOR: OnceLock<SnowflakeGen> = OnceLock::new();

pub fn get_position_generator() -> &'static SnowflakeGen {
    MESSAGE_POSITION_GENERATOR.get_or_init(|| {
        // Use machine ID 0 for now - in production this would be configurable
        let clock = ferroid::MonotonicClock::with_epoch(ferroid::TWITTER_EPOCH);
        ferroid::AtomicSnowflakeGenerator::new(0, clock)
    })
}

/// Get the next message position synchronously
///
/// This is designed for use in synchronous contexts like Default impls.
/// In practice, we don't generate messages fast enough to hit the sequence
/// limit (65536/ms), so Pending should rarely happen in production.
///
/// When the sequence is exhausted (e.g., in parallel tests), this will block
/// briefly until the next millisecond boundary to get a fresh sequence.
pub fn get_next_message_position_sync() -> SnowflakePosition {
    use ferroid::IdGenStatus;

    let generator = get_position_generator();

    loop {
        match generator.next_id() {
            IdGenStatus::Ready { id } => return SnowflakePosition::new(id),
            IdGenStatus::Pending { yield_for } => {
                // If yield_for is 0, we're at the sequence limit but still in the same millisecond.
                // Wait at least 1ms to roll over to the next millisecond and reset the sequence.
                let wait_ms = yield_for.max(1) as u64;
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                // Loop will retry after the wait
            }
        }
    }
}

/// Get the next message position as a Snowflake ID (async version)
pub async fn get_next_message_position() -> SnowflakePosition {
    let id = get_position_generator()
        .try_next_id_async()
        .await
        .expect("for now we are assuming this succeeds");
    SnowflakePosition::new(id)
}

/// Get the next message position as a String (for database storage)
pub async fn get_next_message_position_string() -> String {
    get_next_message_position().await.to_string()
}
