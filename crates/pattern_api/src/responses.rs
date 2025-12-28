//! API response types

use pattern_core::{
    agent::{AgentState, AgentType},
    coordination::{CoordinationPattern, GroupMemberRole},
    id::{AgentId, GroupId, MessageId, UserId},
    messages::{ChatRole, MessageContent},
};
use serde::{Deserialize, Serialize};

/// Authentication response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    /// Access token for API requests
    pub access_token: String,
    /// Refresh token for getting new access tokens
    pub refresh_token: String,
    /// Token type (usually "Bearer")
    pub token_type: String,
    /// Expiration time in seconds
    pub expires_in: u64,
    /// User information
    pub user: UserResponse,
}

/// User response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserResponse {
    pub id: UserId,
    pub username: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub is_active: bool,
}

/// Agent response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub id: AgentId,
    pub name: String,
    pub agent_type: AgentType,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub state: AgentState,
    pub model_provider: String,
    pub model_id: String,
    pub tools: Vec<String>,
    pub owner_id: UserId,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub last_active_at: Option<chrono::DateTime<chrono::Utc>>,
    pub message_count: u64,
    pub metadata: Option<serde_json::Value>,
}

/// Group response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupResponse {
    pub id: GroupId,
    pub name: String,
    pub description: String,
    pub coordination_pattern: CoordinationPattern,
    pub owner_id: UserId,
    pub member_count: u32,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub last_active_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Group member response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberResponse {
    pub agent_id: AgentId,
    pub agent_name: String,
    pub role: GroupMemberRole,
    pub capabilities: Vec<String>,
    pub joined_at: chrono::DateTime<chrono::Utc>,
    pub is_active: bool,
    pub metadata: Option<serde_json::Value>,
}

/// Group with members response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupWithMembersResponse {
    #[serde(flatten)]
    pub group: GroupResponse,
    pub members: Vec<GroupMemberResponse>,
}

/// Message response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageResponse {
    pub id: MessageId,
    pub agent_id: AgentId,
    pub role: ChatRole,
    pub content: MessageContent,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tool_calls: Option<Vec<ToolCallResponse>>,
    pub tool_results: Option<Vec<ToolResultResponse>>,
    pub metadata: Option<serde_json::Value>,
}

/// Tool call response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResponse {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool result response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultResponse {
    pub tool_call_id: String,
    pub result: serde_json::Value,
    pub is_error: bool,
}

/// Chat response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub messages: Vec<MessageResponse>,
    pub usage: Option<UsageInfo>,
}

/// Usage information for model calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub model: String,
}

/// Memory response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResponse {
    pub agent_id: AgentId,
    pub memory_type: String,
    pub content: serde_json::Value,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub version: u32,
}

/// Archival memory item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivalMemoryItem {
    pub id: String,
    pub label: String,
    pub content: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub metadata: Option<serde_json::Value>,
}

/// Stats response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub total_users: u64,
    pub active_users: u64,
    pub total_agents: u64,
    pub active_agents: u64,
    pub total_groups: u64,
    pub total_messages: u64,
    pub messages_today: u64,
    pub messages_this_week: u64,
    pub messages_this_month: u64,
}

/// Health check response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: HealthStatus,
    pub version: String,
    pub uptime_seconds: u64,
    pub database_status: ComponentStatus,
    pub services: Vec<ServiceStatus>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub status: ComponentStatus,
    pub message: Option<String>,
}

/// Batch operation response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponse<T> {
    pub successful: Vec<BatchResult<T>>,
    pub failed: Vec<BatchError>,
    pub total_operations: u32,
    pub successful_count: u32,
    pub failed_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult<T> {
    pub index: u32,
    pub result: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchError {
    pub index: u32,
    pub error: String,
}

// ============ MCP Server Management ============

/// MCP server info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerResponse {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub status: McpServerStatus,
    pub transport_type: String,
    pub auto_reconnect: bool,
    pub connected_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
    pub available_tools: Vec<McpToolInfo>,
}

/// MCP server connection status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatus {
    Connected,
    Connecting,
    Disconnected,
    Error,
}

/// MCP tool information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Option<serde_json::Value>,
}

// ============ Model Provider Configuration ============

/// Model provider info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProviderResponse {
    pub id: String,
    pub provider: String,
    pub enabled: bool,
    pub is_default: bool,
    pub status: ModelProviderStatus,
    pub available_models: Vec<ModelInfo>,
    pub last_validated: Option<chrono::DateTime<chrono::Utc>>,
}

/// Model provider status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderStatus {
    Active,
    InvalidCredentials,
    RateLimited,
    Error,
}

/// Model information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_length: u32,
    pub input_cost_per_1k: Option<f32>,
    pub output_cost_per_1k: Option<f32>,
}

// ============ API Key Management ============

/// API key response (only returned on creation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyResponse {
    pub id: String,
    pub name: String,
    pub key: String, // Only returned once on creation
    pub permissions: Vec<crate::requests::ApiPermission>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub rate_limit: Option<u32>,
}

/// API key info (for listing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub key_prefix: String, // First 8 chars of key
    pub permissions: Vec<crate::requests::ApiPermission>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub rate_limit: Option<u32>,
    pub enabled: bool,
}
